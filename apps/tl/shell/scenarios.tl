use so.tilo.chat as chat
use so.tilo.notes as notes
use so.tilo.tasks as tasks

/// Scenarios: the shell as people meet it, in the states its views are for. Each fills
/// the layout's own fields (the ones the session writes, which a scenario may set) and the
/// resources the session's origin carries, so every surface is staged as a screenshot and laid
/// out by `tilolang layout-check` with nothing running.
///
/// They are seed data for shots, tests and benchmarks, never a person's: a real first launch
/// starts empty. They stage the places over notes and chat documents, which is why only they use
/// those apps: the shell itself draws any app's documents through that app's own views.
/// An afternoon of work: three workspaces, four strips of the one in front with windows in
/// each, two people working alongside and a task running. The shell at rest.
scenario "an afternoon of work" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    d.workspaces.insert(LayoutWorkspace(key: "home", name: "Home"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.workspaces.insert(LayoutWorkspace(key: "release", name: "Release 4.2"))
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    d.people.insert(LayoutPerson(key: "sam", name: "Sam Okafor"))
    stage(
        device.agents,
        device.Agents(
            sessions: [
                device.Agent(
                    key: "fieldwork",
                    name: "Fieldwork",
                    state: "running",
                    doing: "Sorting the inbox",
                    mark: "3 threads filed",
                    unseen: 3,
                ),
            ],
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "w2",
                    title: "Launch checklist",
                    origin_app: "Tasks",
                    app_key: "so.tilo.tasks",
                    icon: "list-checks",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w3",
                    title: "Screenshots for the docs",
                    origin_app: "Gallery",
                    app_key: "so.tilo.gallery",
                    icon: "layout_grid",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w4",
                    title: "Interview notes",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 4,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w5",
                    title: "Design review",
                    origin_app: "Figma",
                    app_key: "org.figma.figma",
                    icon: "pen_tool",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 5,
                    focused: false,
                    preview: "withheld",
                ),
                device.Window(
                    key: "w6",
                    title: "Q3 budget",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    recency: 6,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w7",
                    title: "Supplier quotes",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    recency: 7,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w8",
                    title: "Pricing model",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    recency: 8,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 9,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w10",
                    title: "From Ada",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 10,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w11",
                    title: "Release notes draft",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 11,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w12",
                    title: "Competitor scan",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    recency: 12,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w13",
                    title: "Customer calls",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    recency: 13,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w14",
                    title: "Type specimens",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    recency: 14,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w15",
                    title: "Supplier quotes",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Home",
                    recency: 15,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w16",
                    title: "Release board",
                    origin_app: "Tasks",
                    app_key: "so.tilo.tasks",
                    icon: "list-checks",
                    strip: "Drafts",
                    workspace: "Release 4.2",
                    recency: 16,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(
        device.notifications,
        device.Notifications(
            notices: [
                mia_mention(),
                threads_filed(),
                release_moved(),
                build_failed(),
                notebook_shared(),
                backup_done(),
            ],
        ),
    )
}

/// The overview of that layout: the strips of the workspace in front, their windows as places.
scenario "the overview" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:31 PM"))
    stage(device.overlays, device.Overlays(here: "overview"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.workspaces.insert(LayoutWorkspace(key: "home", name: "Home"))
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    stage(
        device.agents,
        device.Agents(
            sessions: [
                device.Agent(
                    key: "fieldwork",
                    name: "Fieldwork",
                    state: "running",
                    doing: "Sorting the inbox",
                    mark: "3 threads filed",
                    unseen: 3,
                ),
            ],
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g1",
                    tab_index: 0,
                    share: 0.5,
                    place: 1,
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "w2",
                    title: "Launch checklist",
                    origin_app: "Tasks",
                    app_key: "so.tilo.tasks",
                    icon: "list-checks",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g2",
                    tab_index: 0,
                    share: 0.5,
                    place: 2,
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w3",
                    title: "Screenshots for the docs",
                    origin_app: "Gallery",
                    app_key: "so.tilo.gallery",
                    icon: "layout_grid",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g2",
                    tab_index: 1,
                    share: 0.5,
                    place: 3,
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w6",
                    title: "Q3 budget",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    strip_key: "money",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g3",
                    tab_index: 0,
                    share: 1.0,
                    place: 4,
                    recency: 6,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    strip_key: "mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g4",
                    tab_index: 0,
                    share: 1.0,
                    place: 5,
                    recency: 9,
                    focused: false,
                    preview: "live",
                    not_responding: true,
                ),
                device.Window(
                    key: "w15",
                    title: "Supplier quotes",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    strip_key: "money",
                    workspace: "Home",
                    workspace_key: "home",
                    group_key: "g5",
                    tab_index: 0,
                    share: 1.0,
                    place: 6,
                    recency: 15,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
}

/// The same overview opened while the person's last search still stands: what that search
/// found across the other workspaces.
scenario "the overview, searching" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:31 PM"))
    stage(device.overlays, device.Overlays(here: "overview", spotlight: "quotes"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.workspaces.insert(LayoutWorkspace(key: "home", name: "Home"))
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    stage(
        device.agents,
        device.Agents(
            sessions: [
                device.Agent(
                    key: "fieldwork",
                    name: "Fieldwork",
                    state: "running",
                    doing: "Sorting the inbox",
                    mark: "3 threads filed",
                    unseen: 3,
                ),
            ],
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g1",
                    tab_index: 0,
                    share: 0.5,
                    place: 1,
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "w2",
                    title: "Launch checklist",
                    origin_app: "Tasks",
                    app_key: "so.tilo.tasks",
                    icon: "list-checks",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g2",
                    tab_index: 0,
                    share: 0.5,
                    place: 2,
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w3",
                    title: "Screenshots for the docs",
                    origin_app: "Gallery",
                    app_key: "so.tilo.gallery",
                    icon: "layout_grid",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g2",
                    tab_index: 1,
                    share: 0.5,
                    place: 3,
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w6",
                    title: "Q3 budget",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    strip_key: "money",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g3",
                    tab_index: 0,
                    share: 1.0,
                    place: 4,
                    recency: 6,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    strip_key: "mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "g4",
                    tab_index: 0,
                    share: 1.0,
                    place: 5,
                    recency: 9,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w15",
                    title: "Supplier quotes",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    strip_key: "money",
                    workspace: "Home",
                    workspace_key: "home",
                    group_key: "g5",
                    tab_index: 0,
                    share: 1.0,
                    place: 6,
                    recency: 15,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w17",
                    title: "Quotes for the printer",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    strip_key: "drafts",
                    workspace: "Home",
                    workspace_key: "home",
                    group_key: "g6",
                    tab_index: 0,
                    share: 1.0,
                    place: 7,
                    recency: 17,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
}

/// The switcher with two windows: what Alt+Tab shows once it is held past the flick.
scenario "the switcher, two windows" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(
        device.overlays,
        device.Overlays(here: "switcher", switcher_revealed: true, switcher_scope: "workspace"),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "w9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
}

/// The switcher with twenty windows: the walk does not fit one row.
scenario "the switcher, twenty windows" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(
        device.overlays,
        device.Overlays(here: "switcher", switcher_revealed: true, switcher_scope: "workspace"),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "s1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "s2",
                    title: "Launch checklist",
                    origin_app: "Tasks",
                    app_key: "so.tilo.tasks",
                    icon: "list-checks",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s3",
                    title: "Screenshots for the docs",
                    origin_app: "Gallery",
                    app_key: "so.tilo.gallery",
                    icon: "layout_grid",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s4",
                    title: "Interview notes",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 4,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s5",
                    title: "Design review",
                    origin_app: "Figma",
                    app_key: "org.figma.figma",
                    icon: "pen_tool",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 5,
                    focused: false,
                    preview: "withheld",
                ),
                device.Window(
                    key: "s6",
                    title: "Q3 budget",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 6,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s7",
                    title: "Supplier quotes",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 7,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s8",
                    title: "Pricing model",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 8,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 9,
                    focused: false,
                    preview: "live",
                    not_responding: true,
                ),
                device.Window(
                    key: "s10",
                    title: "From Ada",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 10,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s11",
                    title: "Release notes draft",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 11,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s12",
                    title: "Competitor scan",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 12,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s13",
                    title: "Customer calls",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 13,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s14",
                    title: "Type specimens",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 14,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s15",
                    title: "Board minutes",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 15,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s16",
                    title: "Onboarding script",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 16,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s17",
                    title: "Weekly sync",
                    origin_app: "Chat",
                    app_key: "so.tilo.chat",
                    icon: "message_circle",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 17,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s18",
                    title: "Architecture note",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 18,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s19",
                    title: "Perf numbers",
                    origin_app: "Charts",
                    app_key: "so.tilo.charts",
                    icon: "chart_line",
                    strip: "Money",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 19,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "s20",
                    title: "Style guide",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 20,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
}

/// The switcher on this app's windows: Alt+` walks the four Notes windows and nothing else, and
/// the panel says which app and how far along them the walk is.
scenario "the switcher, this app's windows" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(
        device.overlays,
        device.Overlays(here: "switcher", switcher_revealed: true, switcher_scope: "app"),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "n1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "n2",
                    title: "Interview notes",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "n3",
                    title: "Competitor scan",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "n4",
                    title: "Type specimens",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Research",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 4,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "m1",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 5,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "m2",
                    title: "From Ada",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    workspace_key: "studio",
                    recency: 6,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
}

/// The launcher an empty query shows: the applications installed here, recent windows and the
/// shell's surfaces.
/// An empty shell: a workspace with no window in it, and the start cards a person meets there.
scenario "an empty shell's start cards" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(device.apps, device.Apps(installed: installed_apps()))
    stage(device.windows, device.Windows(all: []))
}

/// The shell's passing notice over the windows: a close said with the Undo that reopens it.
scenario "a passing notice with Undo" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.said,
        device.Said(
            text: "Closed Release planning",
            kind: "info",
            actions: [device.SaidAction(id: "reopen", name: "Undo", icon: "undo-2")],
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
}

scenario "the launcher, before typing" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "launcher"))
    stage(device.apps, device.Apps(installed: installed_apps()))
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "w9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w6",
                    title: "Q3 budget",
                    origin_app: "Sheets",
                    app_key: "so.tilo.sheets",
                    icon: "table",
                    strip: "Money",
                    workspace: "Studio",
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(device.search, device.Search(groups: []))
    stage(
        device.launchpad,
        device.Launchpad(
            cards: [
                device.LaunchpadCard(slug: "notes", label: "Open Notes", icon: "notebook"),
                device.LaunchpadCard(slug: "invite", label: "Invite someone", icon: "user-plus"),
                device.LaunchpadCard(slug: "activity", label: "Catch up", icon: "history"),
            ],
        ),
    )
}

/// The launcher with "mai" typed: the apps and the windows that match.
scenario "the launcher, mail typed" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "launcher", spotlight: "mai"))
    stage(device.apps, device.Apps(installed: installed_apps()))
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w9",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 1,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w10",
                    title: "From Ada",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w11",
                    title: "Mailbox rules",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Mail",
                    workspace: "Studio",
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 4,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(device.search, device.Search(groups: []))
    stage(
        device.launchpad,
        device.Launchpad(
            cards: [
                device.LaunchpadCard(slug: "notes", label: "Open Notes", icon: "notebook"),
                device.LaunchpadCard(slug: "invite", label: "Invite someone", icon: "user-plus"),
                device.LaunchpadCard(slug: "activity", label: "Catch up", icon: "history"),
            ],
        ),
    )
}

/// A command palette query: an action, named the way people name it.
scenario "the launcher, an action typed" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "launcher", spotlight: "focus"))
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Focus: the launch",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(
        device.search,
        device.Search(
            query: "focus",
            open: true,
            groups: [
                device.SearchGroup(
                    kind: "Actions",
                    icon: "settings-2",
                    rows: [
                        device.SearchResult(
                            index: 0,
                            title: "Focus the window in front",
                            subtitle: "Window",
                        ),
                    ],
                ),
                device.SearchGroup(
                    kind: "Files",
                    icon: "file",
                    rows: [
                        device.SearchResult(
                            index: 1,
                            title: "Launch notes.md",
                            subtitle: "Studio / Notes",
                            facts: ["Edited today"],
                        ),
                    ],
                ),
            ],
        ),
    )
    stage(device.launchpad, device.Launchpad(cards: []))
}

/// The notification centre with fifteen notices from apps and from agent tasks.
scenario "the notification centre" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    stage(
        device.notifications,
        device.Notifications(
            notices: [
                mia_mention(),
                threads_filed(),
                release_moved(),
                build_failed(),
                notebook_shared(),
                backup_done(),
                device.Notice(
                    number: 7,
                    title: "Screenshot ready",
                    body: "The capture is in Pictures.",
                    from: "Capture Studio",
                    kind: "info",
                    seen: true,
                ),
                device.Notice(
                    number: 8,
                    title: "Ada replied in Chat",
                    body: "Let's ship Friday.",
                    from: "Chat",
                    kind: "info",
                    seen: false,
                ),
                device.Notice(
                    number: 9,
                    title: "Your laptop is at 12%",
                    body: "Plug it in to finish the backup.",
                    from: "Power",
                    kind: "warning",
                    seen: true,
                ),
                device.Notice(
                    number: 10,
                    title: "Fieldwork filed 40 threads",
                    body: "The mailbox is tidy.",
                    from: "Fieldwork",
                    kind: "info",
                    seen: true,
                ),
                device.Notice(
                    number: 11,
                    title: "New member in Studio",
                    body: "Sam Okafor joined.",
                    from: "Studio",
                    kind: "info",
                    seen: true,
                ),
                device.Notice(
                    number: 12,
                    title: "The release board moved to Review",
                    body: "Jo moved 6 tasks.",
                    from: "Release 4.2",
                    kind: "info",
                    seen: false,
                ),
                device.Notice(
                    number: 13,
                    title: "Backup finished",
                    body: "12 GB of changes are safe.",
                    from: "Settings",
                    kind: "info",
                    seen: true,
                ),
                device.Notice(
                    number: 14,
                    title: "Calendar: Standup in 10 minutes",
                    body: "Studio room.",
                    from: "Calendar",
                    kind: "info",
                    seen: false,
                ),
                device.Notice(
                    number: 15,
                    title: "Fieldwork is waiting for you",
                    body: "It asks which folder to archive.",
                    from: "Fieldwork",
                    kind: "warning",
                    seen: false,
                    actions: [device.NoticeAction(key: "open", label: "Answer")],
                ),
            ],
        ),
    )
}

/// The centre with nothing new: every notice read, so its header says so and no row carries a
/// mark, and one group's choices with it.
scenario "the notification centre, all read" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    stage(
        device.notifications,
        device.Notifications(
            notices: [
                device.Notice(
                    number: 1,
                    title: "Mia mentioned you in Release planning",
                    body: "Can you check the launch date?",
                    from: "Notes",
                    kind: "info",
                    seen: true,
                    actions: [
                        device.NoticeAction(key: "open", label: "Reply"),
                        device.NoticeAction(key: "later", label: "Later"),
                    ],
                ),
                device.Notice(
                    number: 2,
                    title: "Three threads filed",
                    body: "Fieldwork filed 3 threads into Receipts.",
                    from: "Fieldwork",
                    kind: "info",
                    seen: true,
                ),
                device.Notice(
                    number: 3,
                    title: "Release 4.2 moved to Review",
                    body: "Jo moved the board on.",
                    from: "Tasks",
                    kind: "info",
                    seen: true,
                ),
            ],
        ),
    )
}

/// The quick settings over the shell, on a computer that publishes no sound, display or
/// battery: the tiles stand, and the sliders and the battery line wait for what is there.
scenario "the quick settings" {
    let d = root Layout
    stage(device.overlays, device.Overlays(here: "quick-settings"))
}

/// The quick settings on a laptop at work: a song playing, the sound and its microphone, the
/// networks and the Bluetooth devices in reach, a VPN profile, the night light, the power mode and
/// the charge.
scenario "the quick settings, at work" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.overlays, device.Overlays(here: "quick-settings"))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(
        device.battery,
        device.Battery(present: true, percentage: 76, state: "discharging", on_battery: true),
    )
    stage(device.power, device.Power(modes: [.saver, .balanced, .performance], mode: .balanced))
    stage(device.night_light, device.NightLight(on: false))
    stage(
        device.vpn,
        device.Vpn(
            profiles: [
                device.VpnProfile(name: "Work", id: "work", kind: "wireguard", connected: true),
            ],
        ),
    )
    stage(
        device.sound,
        device.Sound(
            volume: 0.62,
            muted: false,
            input_volume: 0.8,
            input_muted: false,
            outputs: [
                device.Device(
                    node: 1,
                    name: "alsa_output.pci",
                    label: "Studio headphones",
                    chosen: true,
                    channels: 2,
                ),
            ],
            inputs: [
                device.Device(
                    node: 2,
                    name: "alsa_input.usb",
                    label: "USB microphone",
                    chosen: true,
                    channels: 1,
                ),
            ],
            uses: [
                device.Use(
                    client_id: 7,
                    pid: 4242,
                    app_id: "so.tilo.meet",
                    label: "Meet",
                    binary: "meet",
                    kind: .microphone,
                ),
            ],
        ),
    )
    stage(
        device.network,
        device.Network(
            airplane_mode: false,
            wifi: device.Wifi(
                available: true,
                on: true,
                networks: [
                    device.WifiNetwork(
                        name: "Studio",
                        signal: 4,
                        secure: true,
                        known: true,
                        connected: true,
                    ),
                    device.WifiNetwork(
                        name: "Cafe",
                        signal: 2,
                        secure: false,
                        known: false,
                        connected: false,
                    ),
                ],
            ),
        ),
    )
    stage(
        device.bluetooth,
        device.Bluetooth(
            available: true,
            on: true,
            accessories: [
                device.Accessory(
                    name: "Sony WH-1000XM5",
                    address: "AC:80:0A:11:22:33",
                    battery: 82,
                    connected: true,
                ),
            ],
            nearby: [device.Nearby(name: "Keyboard", address: "AA:BB:CC:DD:EE:FF", signal: -54)],
        ),
    )
}

/// Three banners, newest first, two with actions and one offering a reply; and more waiting behind
/// them than fit.
scenario "the banners" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "2:04 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.notifications,
        device.Notifications(
            notices: [
                build_failed(),
                mia_mention(),
                release_moved(),
                notebook_shared(),
                backup_done(),
            ],
            banners: [4, 1, 3, 5, 6],
        ),
    )
}

/// The hardware keys' readout: the volume key answered with the level it set.
scenario "the readout, volume" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.overlays, device.Overlays(here: "none"))
    stage(device.time, device.Time(clock: "2:04 PM"))
    stage(
        device.osd,
        device.Osd(
            visible: true,
            kind: "volume",
            level: 0.65,
            on: true,
            name: "Studio headphones",
            glyph: "headphones",
        ),
    )
}

/// The hardware keys' readout: the microphone muted, said in words.
scenario "the readout, microphone muted" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.overlays, device.Overlays(here: "none"))
    stage(device.osd, device.Osd(visible: true, kind: "microphone", on: false, name: "Microphone"))
}

/// The bar with a timer running: its activity pill, the capture indicator, a tray item counting
/// its app's unread notices, and an agent at work.
scenario "the bar's work" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.tray,
        device.Tray(
            items: [
                device.TrayItem(
                    key: "chat",
                    title: "Chat",
                    menu: false,
                    badge: 3,
                    attention: false,
                ),
            ],
        ),
    )
    stage(
        device.agents,
        device.Agents(
            sessions: [
                device.Agent(
                    key: "a1",
                    name: "Fieldwork",
                    state: "running",
                    doing: "Reading mail",
                    unseen: 2,
                ),
            ],
        ),
    )
    stage(
        device.sound,
        device.Sound(
            volume: 0.5,
            uses: [
                device.Use(
                    client_id: 7,
                    pid: 4242,
                    app_id: "so.tilo.meet",
                    label: "Meet",
                    binary: "meet",
                    kind: .camera,
                ),
            ],
        ),
    )
}

/// The centre with nothing in it: what a person sees after Clear all.
scenario "an empty notification centre" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
}

/// The centre just after Clear all, while the cleared notices can still come back: the Undo band
/// under the empty list.
scenario "the notification centre, cleared" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    stage(device.notifications, device.Notifications(notices: [], undo: true))
}

/// The workspaces: every one the person is in, and who is here in the one in front.
scenario "the workspaces" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.time, device.Time(clock: "9:12 AM"))
    stage(device.overlays, device.Overlays(here: "workspaces"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.workspaces.insert(LayoutWorkspace(key: "home", name: "Home"))
    d.workspaces.insert(LayoutWorkspace(key: "release", name: "Release 4.2"))
    d.workspaces.insert(LayoutWorkspace(key: "garden", name: "Garden"))
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    d.people.insert(LayoutPerson(key: "sam", name: "Sam Okafor"))
}

/// The phone's shell, signed out: the page asks for the ticket another device printed.
scenario "the phone, signed out" {
    stage(device.devices, device.Devices(signed_in: false))
}

/// The phone's shell with the person signed in, and two workspaces to choose from: picking one
/// shows that workspace's apps, each opening as it does in the launcher.
scenario "the phone, two workspaces" {
    stage(
        device.devices,
        device.Devices(
            signed_in: true,
            workspaces: [
                device.DeviceWorkspace(key: "studio", name: "Studio"),
                device.DeviceWorkspace(key: "home", name: "Home"),
            ],
        ),
    )
    stage(device.apps, device.Apps(installed: installed_apps()))
}

/// A banner over the shell: a build failure with its actions, above a shell at work.
scenario "a banner" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "2:04 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(
        device.notifications,
        device.Notifications(
            notices: [
                device.Notice(
                    number: 4,
                    title: "Build failed",
                    body: "The link check found 4 broken links.",
                    from: "Forgejo",
                    kind: "error",
                    seen: false,
                    actions: [
                        device.NoticeAction(key: "open", label: "Open the run"),
                        device.NoticeAction(key: "retry", label: "Retry"),
                    ],
                ),
            ],
            banner: 4,
        ),
    )
}

/// The bar's status and who is here: the faces of the two people working alongside Ada, the
/// device's sound, connection and charge, the person's own avatar and the notifications with
/// what waits in them.
scenario "the bar's status" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    d.people.insert(LayoutPerson(key: "sam", name: "Sam Okafor"))
}

/// Whether the files in a workspace's folder reached its server, on the bar: in sync.
scenario "the folders, in sync" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.folders,
        device.Folders(
            folders: [
                device.Folder(
                    workspace: "studio",
                    name: "Studio",
                    server: "ada-laptop-server",
                    state: "in-sync",
                    stuck: "",
                    files: 1240,
                ),
            ],
        ),
    )
}

/// Whether the files in a workspace's folder reached its server, on the bar: sending.
scenario "the folders, sending" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.folders,
        device.Folders(
            folders: [
                device.Folder(
                    workspace: "studio",
                    name: "Studio",
                    server: "ada-laptop-server",
                    state: "sending",
                    stuck: "",
                    files: 1240,
                ),
                device.Folder(
                    workspace: "home",
                    name: "Home",
                    server: "ada-laptop-server",
                    state: "in-sync",
                    stuck: "",
                    files: 1240,
                ),
            ],
        ),
    )
}

/// Whether the files in a workspace's folder reached its server, on the bar: one stuck.
scenario "the folders, one stuck" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.folders,
        device.Folders(
            folders: [
                device.Folder(
                    workspace: "studio",
                    name: "Studio",
                    server: "ada-laptop-server",
                    state: "stuck",
                    stuck: "refused",
                    refused: "history-full",
                    files: 1240,
                ),
                device.Folder(
                    workspace: "home",
                    name: "Home",
                    server: "ada-laptop-server",
                    state: "in-sync",
                    stuck: "",
                    files: 1240,
                ),
            ],
        ),
    )
}

/// Whether the files in a workspace's folder reached its server, on the bar: files not on the server yet.
scenario "the folders, files not on the server yet" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.folders,
        device.Folders(
            folders: [
                device.Folder(
                    workspace: "studio",
                    name: "Studio",
                    server: "ada-laptop-server",
                    state: "content-going-out",
                    stuck: "",
                    files: 1240,
                ),
            ],
        ),
    )
}

/// Whether the files in a workspace's folder reached its server, on the bar: a long server name.
scenario "the folders, a long server name" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.folders,
        device.Folders(
            folders: [
                device.Folder(
                    workspace: "studio",
                    name: "Release planning for the autumn launch",
                    server: "the shared storage server in the Hamburg office basement",
                    state: "content-going-out",
                    stuck: "",
                    files: 1240,
                ),
                device.Folder(
                    workspace: "home",
                    name: "Home",
                    server: "the shared storage server in the Hamburg office basement",
                    state: "in-sync",
                    stuck: "",
                    files: 1240,
                ),
            ],
        ),
    )
}

/// Who is here, and the menu a person's presence opens.
scenario "a person's menu" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    d.people.insert(LayoutPerson(key: "sam", name: "Sam Okafor"))
    stage(
        device.agents,
        device.Agents(
            sessions: [
                device.Agent(
                    key: "fieldwork",
                    name: "Fieldwork",
                    state: "running",
                    doing: "Sorting the inbox",
                    mark: "3 threads filed",
                    unseen: 3,
                ),
            ],
        ),
    )
}

/// Adds a workspace, with one row, to `a`, and returns the row's key.
action a_workspace(a: Layout, name: Text) -> Text {
    let w = a.workspaces.insert(LayoutWorkspace(key: "w-{name}", name: name))
    let r = a.rows.insert(
        LayoutRow(key: "r-{name}", workspace: w.key, name: name, shell: "so.tilo.shell.strips"),
    )
    r.key
}

/// Adds the workspace in front, with its row in front, to `a`, and returns the row's key.
action a_front_workspace(a: Layout, name: Text) -> Text {
    let w = a.workspaces.insert(LayoutWorkspace(key: "w-{name}", name: name, current: true))
    let r = a.rows.insert(
        LayoutRow(
            key: "r-{name}",
            workspace: w.key,
            name: name,
            shell: "so.tilo.shell.strips",
            current: true,
        ),
    )
    r.key
}

/// Adds a strip named `name` to `row` at `width`, and returns its key.
action a_strip(a: Layout, row: Text, name: Text, width: Int) -> Text {
    let s = a.strips.insert(
        LayoutStrip(
            key: "s-{row}-{name}",
            workspace: "w-Main",
            row: row,
            width: width,
            shell: "so.tilo.shell.strips",
        ),
    )
    s.key
}

/// Adds a place showing the note at `index` of the notebook to `strip`, and returns its key.
action a_note_place(a: Layout, strip: Text, index: Int) -> Text {
    let chosen = notes.notebook.notes.take(index + 1)
    if let n = chosen.last() {
        let p = a.places.insert(
            LayoutPlace(
                key: "p-{index}",
                strip: strip,
                shows: "so.tilo.notes",
                resource: n.id(),
                title: n.title,
                name: "{n.title} — Notes",
                shell: "so.tilo.shell.strips",
            ),
        )
        return p.key
    }
    ""
}

/// A place of a kind this build has no document for: it stands as it is, which is what a place
/// another shell made looks like.
action a_foreign_place(a: Layout, strip: Text, index: Int, title: Text) -> Text {
    let p = a.places.insert(
        LayoutPlace(
            key: "f-{index}",
            strip: strip,
            shows: "so.tilo.terminal",
            title: title,
            name: "{title} — Terminal",
            shell: "so.tilo.shell.other",
        ),
    )
    p.key
}

/// Adds a place showing a search for `text` to `strip`, with the two matches the notes app
/// answered, and returns the place's key: the state a place stands in until the person chooses
/// one of the resources it found (shell.md §8).
action a_query_place(a: Layout, strip: Text, text: Text) -> Text {
    let p = a.places.insert(
        LayoutPlace(
            key: "q-{strip}",
            strip: strip,
            title: "Search: {text}",
            name: "Search: {text}",
            shell: "so.tilo.shell.strips",
        ),
    )
    a.queries.insert(
        LayoutQuery(
            key: p.key,
            text: text,
            in_app: "so.tilo.notes",
            kind: "notes.note",
            state: "several",
        ),
    )
    for n in notes.notebook.notes.take(2) {
        a.refs.insert(
            LayoutRef(
                key: "m-{n.title}",
                query: p.key,
                address: n.id(),
                title: n.title,
                kind: "notes.note",
            ),
        )
    }
    p.key
}

/// The applications installed on this device, as the launcher lists them: the ones the shell's
/// shots and its layouts show.
fn installed_apps() -> List<device.App> {
    return [
        device.App(key: "so.tilo.notes", name: "Notes", icon: "file-text", category: "work"),
        device.App(
            key: "so.tilo.chat",
            name: "Chat",
            icon: "message-square",
            category: "work",
        ),
        device.App(key: "so.tilo.tasks", name: "Tasks", icon: "list-checks", category: "work"),
        device.App(key: "so.tilo.settings", name: "Settings", icon: "settings", category: "system"),
    ]
}

/// The agent models Settings publishes (`settings.agent_models`): two providers a person let agents
/// use, and the model they answer with. A scenario that shows the canvas' composer stages it, so the
/// model the composer switches stands beside the line the person types on.
fn agent_models() -> settings.AgentModels {
    return settings.AgentModels(
        providers: [
            settings.AgentProviderShown(
                id: "anthropic",
                label: "Anthropic",
                enabled: true,
                models: [
                    settings.AgentModelShown(id: "claude", label: "Claude"),
                    settings.AgentModelShown(id: "claude-haiku", label: "Claude Haiku"),
                ],
                model: "claude",
                model_label: "Claude",
            ),
            settings.AgentProviderShown(
                id: "local",
                label: "On this device",
                enabled: true,
                models: [settings.AgentModelShown(id: "small", label: "Small")],
                model: "small",
                model_label: "Small",
            ),
        ],
        in_use: settings.AgentModelInUse(
            provider: "anthropic",
            model: "claude",
            model_label: "Claude",
        ),
    )
}

/// A shell of one note in the style named, with that place in front, and the style's own settings
/// open over it: what a person sees who opens them from the bar's style menu.
action a_style_settings(style: Text) -> Layout {
    let v = viewing
    v.style = style
    let a = root Layout
    let row = a_workspace(a, "Main")
    let one = a_strip(a, row, "Note", 667)
    a_note_place(a, one, 0)
    a.front = one
    let sh = root Shell
    sh.settings_open = true
    a
}

/// A working layout's places at the counts people keep: one place on its own, three across a
/// narrow list, a wide board and a usual note, then a busy day as three strips of four. The
/// strips stand side by side in one row, and the narrow list holds focus.
scenario "a layout of places" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let one = a_strip(a, row, "One", 667)
    a_note_place(a, one, 0)
    let narrow = a_strip(a, row, "List", 333)
    a.places.insert(
        LayoutPlace(
            key: "n-1",
            strip: narrow,
            shows: "so.tilo.tasks",
            resource: tasks.tasks.id(),
            title: "Tasks",
            name: "Tasks",
            shell: "so.tilo.shell.strips",
        ),
    )
    let wide = a_strip(a, row, "Board", 1000)
    a_foreign_place(a, wide, 1, "A terminal")
    let usual = a_strip(a, row, "Note", 667)
    a_note_place(a, usual, 1)
    let day_a = a_strip(a, row, "Morning", 667)
    let day_b = a_strip(a, row, "Afternoon", 667)
    let day_c = a_strip(a, row, "Evening", 667)
    let i = 0
    for n in notes.notebook.notes.take(12) {
        let held = if i < 4 { day_a } else if i < 8 { day_b } else { day_c }
        a.places.insert(
            LayoutPlace(
                key: "d-{i}",
                strip: held,
                shows: "so.tilo.notes",
                resource: n.id(),
                title: n.title,
                name: "{n.title} — Notes",
                shell: "so.tilo.shell.strips",
            ),
        )
        i += 1
    }
    a.front = narrow
    a
}

/// A window of several places: a strip holding three notes, the first of them in front, beside a
/// strip of one. The face of every surface that shows a group (§3): one window bar, the places
/// as its tabs, the place in front filling the window under them.
scenario "a window of several places" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let group = a_strip(a, row, "Notes", 667)
    a_note_place(a, group, 0)
    a_note_place(a, group, 1)
    a_note_place(a, group, 2)
    let single = a_strip(a, row, "Terminal", 667)
    a_foreign_place(a, single, 3, "A terminal")
    a.front = group
    a
}

/// One window alone in the shell: a strip with a note in it, filling the room between the bar and
/// the dock as the chrome's lone window fills its strip.
scenario "a window alone in the shell" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let one = a_strip(a, row, "Note", 1000)
    a_note_place(a, one, 0)
    a.front = one
    a
}

/// A strip with nothing on it yet, beside a strip with one place: the state a person starts
/// from when they make a strip.
scenario "an empty strip" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    a_strip(a, row, "Empty", 667)
    let used = a_strip(a, row, "In use", 667)
    a_note_place(a, used, 0)
    a
}

/// A strip made under a window: the first row holds a window and the row made after it holds
/// nothing yet, as a shell does after "New strip".
scenario "a strip made under a window" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let one = a_strip(a, row, "Files", 667)
    a_foreign_place(a, one, 1, "Files")
    let second = a.rows.insert(
        LayoutRow(
            key: "r-second",
            workspace: "w-Main",
            name: "Errands",
            shell: "so.tilo.shell:strips",
        ),
    )
    a_strip(a, second.key, "Empty", 667)
    a.front = one
    a
}

/// A place for a search: what a person typed into it, and the matches the notes app
/// answered, each a press that shows it in the same place (`query_view`).
scenario "a place finding what to show" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let found = a_strip(a, row, "Found", 667)
    a_query_place(a, found, "release")
    let beside = a_strip(a, row, "Note", 667)
    a_note_place(a, beside, 0)
    a.front = found
    a
}

/// The strips style's own settings, over the shell they change: the panel the bar's style menu
/// opens (`style_panel`).
scenario "the strips style's settings" -> Layout {
    a_style_settings("so.tilo.shell:strips")
}

/// The frames style's own settings, over the canvas they change.
scenario "the frames style's settings" -> Layout {
    // The note has its point and size on the canvas, and the styles the session publishes are
    // staged, so the shot shows the style the panel belongs to standing behind it and the switch
    // above the settings listing the canvas beside the other three.
    stage(
        device.styles,
        device.Styles(
            offered: [
                device.Style(
                    style: "so.tilo.shell:strips",
                    package: "so.tilo.shell",
                    name: "strips",
                    line: "Strips",
                    loads: true,
                    reason: "",
                ),
                device.Style(
                    style: "so.tilo.shell:frames",
                    package: "so.tilo.shell",
                    name: "frames",
                    line: "Frames",
                    loads: true,
                    reason: "",
                ),
                device.Style(
                    style: "so.tilo.shell:frameless",
                    package: "so.tilo.shell",
                    name: "frameless",
                    line: "Frameless",
                    loads: true,
                    reason: "",
                ),
                device.Style(
                    style: "so.tilo.shell:desktop",
                    package: "so.tilo.shell",
                    name: "desktop",
                    line: "Desktop",
                    loads: true,
                    reason: "",
                ),
            ],
        ),
    )
    stage(settings.agent_models, agent_models())
    let a = a_style_settings("so.tilo.shell:frames")
    if let strip = a.strips.first() {
        strip.canvas_x = 0
        strip.canvas_y = 0
        strip.canvas_width = 1100
        strip.canvas_height = 1200
        strip.placed = true
    }
    a
}

/// The frameless style's own settings, over the places they change.
scenario "the frameless style's settings" -> Layout {
    // The note has its point on the canvas, so the shot shows the style the panel belongs to
    // standing behind it.
    stage(settings.agent_models, agent_models())
    let a = a_style_settings("so.tilo.shell:frameless")
    if let strip = a.strips.first() {
        strip.canvas_x = 0
        strip.canvas_y = 0
        strip.canvas_width = 1100
        strip.canvas_height = 1200
        strip.placed = true
    }
    a
}

/// The desktop style's own settings, over the shell they change.
scenario "the desktop style's settings" -> Layout {
    a_style_settings("so.tilo.shell:desktop")
}

/// A reference activated from a chat place: the channel's place, and the note's place it landed
/// beside — a column after the one it came from (shell.md §3).
scenario "a reference lands beside its place" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:strips"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let talks = a_strip(a, row, "Chat", 667)
    let design = channel_named("design")
    a.places.insert(
        LayoutPlace(
            key: "ch",
            strip: talks,
            shows: "so.tilo.chat",
            resource: design?.id() ?? "",
            title: "design",
            name: "design — Chat",
            shell: "so.tilo.shell.strips",
        ),
    )
    a_note_place(a, talks, 1)
    // The note's place is inserted by an action of its own, so it stands before the chat place
    // the scenario inserted: the strip says which place is in front.
    if let strip = a.strips.find(s -> s.key == talks) {
        strip.active = "ch"
    }
    a.front = talks
    a
}

/// Six places in two clusters on the framed canvas: one cluster per task, at three zoom levels
/// as the person looks closer or further out. Each note is its app's editor composed in its
/// frame; the terminal beside them is a window, shown in its well.
scenario "a canvas of clusters" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:frames"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let s = a_strip(a, row, "Canvas", 667)
    let strip = a.strips.find(st -> st.key == s)
    if let held = strip {
        held.canvas_x = 0
        held.canvas_y = 0
        held.canvas_width = 1100
        held.canvas_height = 1600
        held.placed = true
    }
    let i = 0
    for n in notes.notebook.notes.take(6) {
        let p = a.places.insert(
            LayoutPlace(
                key: "c-{i}",
                strip: s,
                shows: "so.tilo.notes",
                shown_as: "note_editor",
                resource: n.id(),
                title: n.title,
                name: "{n.title} — Notes",
                shell: "so.tilo.shell.strips",
            ),
        )
        i += 1
    }
    a.places.insert(
        LayoutPlace(
            key: "c-terminal",
            strip: s,
            shows: "so.tilo.terminal",
            title: "A terminal",
            name: "A terminal — Terminal",
            shell: "so.tilo.shell.strips",
        ),
    )
    stage(settings.agent_models, agent_models())
    a
}

/// The frameless canvas: the same places with no frame around them, so a resource's view stands
/// as content and nearness groups it. Each note is its app's editor composed in place; the
/// terminal beside them is a window, shown in its well.
scenario "resources shown as content" -> Layout {
    let v = viewing
    v.style = "so.tilo.shell:frameless"
    let a = root Layout
    let row = a_workspace(a, "Main")
    let s = a_strip(a, row, "Frameless", 667)
    let strip = a.strips.find(st -> st.key == s)
    if let held = strip {
        held.canvas_x = 0
        held.canvas_y = 0
        held.canvas_width = 1100
        held.canvas_height = 1200
        held.placed = true
    }
    let i = 0
    for n in notes.notebook.notes.take(4) {
        a.places.insert(
            LayoutPlace(
                key: "x-{i}",
                strip: s,
                shows: "so.tilo.notes",
                shown_as: "note_editor",
                resource: n.id(),
                title: n.title,
                name: "{n.title} — Notes",
                shell: "so.tilo.shell.strips",
            ),
        )
        i += 1
    }
    a.places.insert(
        LayoutPlace(
            key: "x-terminal",
            strip: s,
            shows: "so.tilo.terminal",
            title: "A terminal",
            name: "A terminal — Terminal",
            shell: "so.tilo.shell.strips",
        ),
    )
    stage(settings.agent_models, agent_models())
    a
}

/// The traditional desktop: the preset furthest from the strips and the canvas, staged with
/// real documents. Three windows stand where the person left them over the shell — a note, the
/// note beside it and a chat channel — the note in front over the corners of the other two as a
/// person working leaves them, each still named by its own bar; the in-front place's own resource publishes the commands the top bar's
/// action strip draws (the notes app's `group:` runs), and the dock rides the foot. The style
/// is the desktop and the chrome preset the traditional one, so this is exactly what a
/// person sees when they pick it.
scenario "the traditional desktop" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    d.workspaces.insert(LayoutWorkspace(key: "home", name: "Home"))
    d.people.insert(LayoutPerson(key: "mia", name: "Mia Clayton"))
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Main",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "w2",
                    title: "Launch checklist",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Main",
                    workspace: "Studio",
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
                device.Window(
                    key: "w3",
                    title: "design",
                    origin_app: "Chat",
                    app_key: "so.tilo.chat",
                    icon: "message-square",
                    strip: "Main",
                    workspace: "Studio",
                    recency: 3,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    apply(s, .windows_desktop)
    let pinned = notes.notebook.notes.filter(n -> n.pinned)
    let rest = notes.notebook.notes.filter(n -> not n.pinned)
    let note = pinned.first() ?? rest.first()
    let beside = pinned.last() ?? rest.last()
    let channel = channel_named("design")
    let v = viewing
    v.style = "so.tilo.shell:desktop"
    let a = root Layout
    let row = a_front_workspace(a, "Main")
    let main = a_strip(a, row, "Main", 667)
    if let held = a.strips.find(st -> st.key == main) {
        held.canvas_x = 380
        held.canvas_y = 230
        held.canvas_width = 600
        held.canvas_height = 480
        held.placed = true
    }
    let behind = a_strip(a, row, "Behind", 667)
    if let held = a.strips.find(st -> st.key == behind) {
        held.canvas_x = 40
        held.canvas_y = 64
        held.canvas_width = 500
        held.canvas_height = 380
        held.placed = true
    }
    let talk = a_strip(a, row, "Talk", 667)
    if let held = a.strips.find(st -> st.key == talk) {
        held.canvas_x = 700
        held.canvas_y = 56
        held.canvas_width = 540
        held.canvas_height = 560
        held.placed = true
    }
    if let n = note {
        a.places.insert(
            LayoutPlace(
                key: "p1",
                strip: main,
                shows: "so.tilo.notes",
                resource: n.id(),
                title: n.title,
                name: "{n.title} — Notes",
                shell: "so.tilo.shell.strips",
            ),
        )
    }
    if let n = beside {
        a.places.insert(
            LayoutPlace(
                key: "p2",
                strip: behind,
                shows: "so.tilo.notes",
                resource: n.id(),
                title: n.title,
                name: "{n.title} — Notes",
                shell: "so.tilo.shell.strips",
            ),
        )
    }
    if let c = channel {
        a.places.insert(
            LayoutPlace(
                key: "p3",
                strip: talk,
                shows: "so.tilo.chat",
                resource: c.id(),
                title: c.name,
                name: "{c.name} — Chat",
                shell: "so.tilo.shell.strips",
            ),
        )
    }
    if let strip = a.strips.find(st -> st.key == main) {
        strip.active = "p1"
    }
    a.front = main
}

/// The traditional desktop with a terminal in front: a window the shell has no document for, so
/// the top bar offers the shell's own commands on it — its name, moving it to another row and
/// closing it — where an app's window would offer the app's.
scenario "the traditional desktop, a terminal in front" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "w1",
                    title: "A terminal",
                    origin_app: "Terminal",
                    app_key: "so.tilo.terminal",
                    icon: "terminal",
                    strip: "Main",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    apply(s, .windows_desktop)
    let v = viewing
    v.style = "so.tilo.shell:desktop"
    let a = root Layout
    let row = a_front_workspace(a, "Main")
    a_workspace(a, "Errands")
    let main = a_strip(a, row, "Main", 667)
    if let held = a.strips.find(st -> st.key == main) {
        held.canvas_x = 120
        held.canvas_y = 80
        held.canvas_width = 640
        held.canvas_height = 420
        held.placed = true
    }
    a_foreign_place(a, main, 1, "A terminal")
    a.front = main
}

/// The default shell with the agents on the bar: the person showed the Agents part, and the
/// two agents working for them stand there, each a chip that opens its menu.
scenario "the agents on the bar" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(
        device.agents,
        device.Agents(
            sessions: [
                device.Agent(
                    key: "fieldwork",
                    name: "Fieldwork",
                    state: "running",
                    doing: "Sorting the inbox",
                    mark: "3 threads filed",
                    unseen: 3,
                ),
                device.Agent(
                    key: "claude",
                    name: "Claude",
                    state: "done",
                    doing: "Tidied the launch notes",
                ),
            ],
        ),
    )
    let s = shell
    if let part = s.bar_parts.find(p -> p.kind == BarPartKind.agent) { show_part(s, part, true) }
}

/// The text channel named `name` in the chat's sidebar, on its own or in a category.
fn channel_named(name: Text) -> Option<chat.TextChannel> {
    for x in chat.conversations.channels {
        match x {
            c: chat.Category => {
                for y in c.channels {
                    match y {
                        t: chat.TextChannel => {
                            if t.name == name {
                                return t
                            }
                        }
                        v: chat.VoiceChannel => {}
                    }
                }
            }
            t: chat.TextChannel => {
                if t.name == name {
                    return t
                }
            }
            v: chat.VoiceChannel => {}
            d: chat.Direct => {}
        }
    }
    .none
}

/// A notice: a board moved on.
fn release_moved() -> device.Notice {
    return device.Notice(
        number: 3,
        title: "Release 4.2 moved to Review",
        body: "Jo moved the board on.",
        from: "Tasks",
        kind: "info",
        seen: true,
    )
}

/// A notice: Mia asking about the launch date.
fn mia_mention() -> device.Notice {
    return device.Notice(
        number: 1,
        title: "Mia mentioned you in Release planning",
        body: "Can you check the launch date?",
        from: "Notes",
        kind: "info",
        seen: false,
        actions: [
            device.NoticeAction(key: "open", label: "Reply"),
            device.NoticeAction(key: "later", label: "Later"),
        ],
    )
}

/// A notice: Fieldwork done filing.
fn threads_filed() -> device.Notice {
    return device.Notice(
        number: 2,
        title: "Three threads filed",
        body: "Fieldwork filed 3 threads into Receipts.",
        from: "Fieldwork",
        kind: "success",
        seen: false,
    )
}

/// A notice: a failed build.
fn build_failed() -> device.Notice {
    return device.Notice(
        number: 4,
        title: "Build failed",
        body: "The link check found 4 broken links.",
        from: "Forgejo",
        kind: "error",
        seen: false,
        actions: [
            device.NoticeAction(key: "open", label: "Open the run"),
            device.NoticeAction(key: "retry", label: "Retry"),
        ],
    )
}

/// A notice: a notebook shared.
fn notebook_shared() -> device.Notice {
    return device.Notice(
        number: 5,
        title: "Ada shared a notebook",
        body: "Launch checklist is now yours to edit.",
        from: "Chat",
        kind: "info",
        seen: true,
    )
}

/// A notice: a backup done.
fn backup_done() -> device.Notice {
    return device.Notice(
        number: 6,
        title: "Backup finished",
        body: "Your laptop backed up 12 GB of changes.",
        from: "Settings",
        kind: "success",
        seen: true,
    )
}

/// The emoji picker over the person's work: the toolkit's card, open where the shell draws the
/// screen, its search ready for the first key.
scenario "the emoji picker" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    d.strips.insert(
        LayoutStrip(
            key: "s-notes",
            workspace: "studio",
            row: "r-drafts",
            width: 667,
            active: "p1",
            placed: false,
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p1",
            strip: "s-notes",
            shows: "so.tilo.notes",
            title: "Release planning",
            name: "Release planning — Notes",
            shell: "so.tilo.shell",
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "p1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(device.overlays, device.Overlays(here: "emoji", emoji: true))
}

/// What this computer is, over the person's work.
scenario "the About sheet" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(
        device.overlays,
        device.Overlays(
            here: "about",
            about: true,
            about_sheet: .some(
                device.AboutSheet(
                    title: "About Tilo",
                    heading: "studio-laptop",
                    version: "Tilo 0.1.0 (6f2c1ab)",
                    home: "https://tilo.so",
                ),
            ),
        ),
    )
}

/// The power menu, under the bar's trailing end: everything this session may do to itself and
/// this computer.
scenario "the power menu" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "power"))
    stage(
        device.power_menu,
        device.PowerMenu(
            open: true,
            offers: ["lock", "log-out", "switch-user", "suspend", "restart", "shut-down"],
        ),
    )
}

/// The power menu in a session that runs in another desktop's window: it owns no machine, so it
/// offers what stays in the session, and shutting down ends only it.
scenario "the power menu in a window" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "power"))
    stage(
        device.power_menu,
        device.PowerMenu(open: true, offers: ["lock", "log-out", "shut-down"]),
    )
}

/// Holding Super: the keys it reaches, said over the person's work and in the way of nothing.
scenario "holding Super" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    d.strips.insert(
        LayoutStrip(
            key: "s-notes",
            workspace: "studio",
            row: "r-drafts",
            width: 667,
            active: "p1",
            placed: false,
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p1",
            strip: "s-notes",
            shows: "so.tilo.notes",
            title: "Release planning",
            name: "Release planning — Notes",
            shell: "so.tilo.shell",
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "p1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(
        device.overlays,
        device.Overlays(
            here: "none",
            peek: .some(
                device.Peek(
                    groups: [
                        device.PeekGroup(
                            title: "Windows",
                            hints: [
                                device.PeekHint(keys: ["Tab"], label: "Next window"),
                                device.PeekHint(keys: ["`"], label: "Its tabs"),
                                device.PeekHint(keys: ["Left"], label: "A window left"),
                                device.PeekHint(keys: ["Right"], label: "A window right"),
                            ],
                        ),
                        device.PeekGroup(
                            title: "Apps",
                            hints: [
                                device.PeekHint(keys: ["E"], label: "Files"),
                                device.PeekHint(keys: ["T"], label: "Terminal"),
                                device.PeekHint(keys: ["Space"], label: "Search"),
                            ],
                        ),
                    ],
                ),
            ),
        ),
    )
}

/// A window whose app stopped answering: its own place says so, with Wait and Force quit, where
/// the person is working.
scenario "a window that stopped answering" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    d.strips.insert(
        LayoutStrip(
            key: "s-notes",
            workspace: "studio",
            row: "r-drafts",
            width: 667,
            active: "p1",
            placed: false,
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p1",
            strip: "s-notes",
            shows: "so.tilo.notes",
            title: "Release planning",
            name: "Release planning — Notes",
            shell: "so.tilo.shell",
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "p1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    recency: 1,
                    focused: true,
                    preview: "live",
                    not_responding: true,
                ),
            ],
        ),
    )
    stage(device.overlays, device.Overlays(here: "none"))
}

/// A window that runs on another device: its place says which app it is, the device it runs on,
/// and that it opens there.
scenario "a window on another device" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    d.strips.insert(
        LayoutStrip(
            key: "s-notes",
            workspace: "studio",
            row: "r-drafts",
            width: 667,
            active: "p1",
            placed: false,
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p1",
            strip: "s-notes",
            shows: "so.tilo.notes",
            title: "Launch checklist",
            name: "Launch checklist — Notes",
            shell: "so.tilo.shell",
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "p1",
                    title: "Launch checklist",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    elsewhere: true,
                    device: "9f2c",
                    device_name: "Mia's laptop",
                    recency: 1,
                    focused: true,
                    preview: "none",
                ),
            ],
        ),
    )
    stage(device.overlays, device.Overlays(here: "none"))
}

/// Reporting a problem: the picker over the person's work, each window of the workspace in front
/// a card with its own picture, and the line saying what a click and a drag do.
scenario "the problem report's picker" -> Layout {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    d.strips.insert(
        LayoutStrip(
            key: "s-notes",
            workspace: "studio",
            row: "r-drafts",
            width: 667,
            active: "p1",
            placed: false,
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p1",
            strip: "s-notes",
            shows: "so.tilo.notes",
            title: "Release planning",
            name: "Release planning — Notes",
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p2",
            strip: "s-notes",
            shows: "so.tilo.mail",
            title: "Inbox",
            name: "Inbox — Mail",
            shell: "so.tilo.shell",
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "p1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    strip_key: "s-notes",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "p1",
                    share: 0.5,
                    local: true,
                    place: 0,
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
                device.Window(
                    key: "p2",
                    title: "Inbox",
                    origin_app: "Mail",
                    app_key: "so.tilo.mail",
                    icon: "mail",
                    strip: "Drafts",
                    strip_key: "s-notes",
                    workspace: "Studio",
                    workspace_key: "studio",
                    group_key: "p2",
                    share: 0.5,
                    local: true,
                    place: 1,
                    recency: 2,
                    focused: false,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "none"))
    let sh = root Shell
    sh.reporting = true
    d
}

/// A press that needs the person's yes: Notes' Share asked before it gives Mia the note, and the
/// question stands over the window it was pressed in, with what it would share and with whom.
scenario "a press that needs a yes" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    d.rows.insert(LayoutRow(key: "r-drafts", workspace: "studio", name: "Drafts", current: true))
    d.strips.insert(
        LayoutStrip(
            key: "s-notes",
            workspace: "studio",
            row: "r-drafts",
            width: 1000,
            active: "p1",
            placed: false,
            shell: "so.tilo.shell",
        ),
    )
    d.places.insert(
        LayoutPlace(
            key: "p1",
            strip: "s-notes",
            shows: "so.tilo.notes",
            title: "Release planning",
            name: "Release planning — Notes",
            shell: "so.tilo.shell",
        ),
    )
    stage(
        device.windows,
        device.Windows(
            all: [
                device.Window(
                    key: "p1",
                    title: "Release planning",
                    origin_app: "Notes",
                    app_key: "so.tilo.notes",
                    icon: "notebook",
                    strip: "Drafts",
                    workspace: "Studio",
                    local: true,
                    device: "laptop",
                    recency: 1,
                    focused: true,
                    preview: "live",
                ),
            ],
        ),
    )
    stage(device.overlays, device.Overlays(here: "none"))
    let a = root Asking
    a.questions.insert(
        Asked(
            key: "studio/invocations/share-1",
            workspace_name: "Studio",
            name: "Share",
            on: "Release planning",
            verb: "share",
            plural: "notes",
            can_always: true,
            by: "app",
            by_name: "Notes",
            author: "Tilo",
            asked_at_ms: 2,
            pressed_device: "laptop",
            pressed_window: "p1",
        ),
    )
    a.facts.insert(
        AskedFact(
            key: "studio/invocations/share-1/0",
            question: "studio/invocations/share-1",
            label: "With",
            value: "Mia Clayton",
        ),
    )
    a.facts.insert(
        AskedFact(
            key: "studio/invocations/share-1/1",
            question: "studio/invocations/share-1",
            label: "Access",
            value: "Can edit",
        ),
    )
}

/// What waits for the person in the notification centre: an agent's question with choices, a
/// device's confirmation that takes away what Undo does not bring back, and a press made on
/// another computer, each under who asks and where, long names and all.
scenario "questions waiting" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    let a = root Asking
    a.questions.insert(
        Asked(
            key: "studio/invocations/ask-1",
            workspace_name: "Studio",
            name: "Answer",
            words: "Which calendar should the launch review go on?",
            by: "agent",
            by_name: "Scout",
            asked_at_ms: 3,
            conversation: "studio/chat/launch",
        ),
    )
    a.choices.insert(
        AskedChoice(key: "studio/invocations/ask-1/0", question: "studio/invocations/ask-1", index: 0, label: "Team"),
    )
    a.choices.insert(
        AskedChoice(key: "studio/invocations/ask-1/1", question: "studio/invocations/ask-1", index: 1, label: "Launch reviews and retrospectives"),
    )
    a.questions.insert(
        Asked(
            key: "home/invocations/forget-1",
            workspace_name: "The Clayton–Lovelace household and everything we keep together",
            name: "Forget this device",
            on: "Ada's old ThinkPad X1 Carbon (6th generation)",
            destructive: true,
            by: "device",
            by_name: "Ada's phone",
            asked_at_ms: 2,
        ),
    )
    a.questions.insert(
        Asked(
            key: "studio/invocations/alarm-1",
            workspace_name: "Studio",
            name: "Set alarm",
            on: "Stand-up",
            verb: "set",
            plural: "alarms",
            can_always: true,
            by: "app",
            by_name: "Clocks",
            author: "Mia Clayton",
            asked_at_ms: 1,
            pressed_device: "desktop",
            pressed_window: "w9",
        ),
    )
    a.facts.insert(
        AskedFact(key: "studio/invocations/alarm-1/0", question: "studio/invocations/alarm-1", label: "At", value: "09:30 every weekday"),
    )
}

/// What Ada opened lately, every app's: her own opens first, then what Mia opened in the
/// workspace, each resource once, under its app.
scenario "the recents" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "history"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    recents_of_an_afternoon(d)
}

/// The recents narrowed to one app, as its recents button opens them.
scenario "the recents of one app" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.time, device.Time(clock: "4:28 PM"))
    stage(device.overlays, device.Overlays(here: "history", history_app: "so.tilo.notes"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    recents_of_an_afternoon(d)
}

/// The recents after Clear: nothing listed until the next open.
scenario "the recents, cleared" {
    let d = root Layout
    d.person = "Ada Lovelace"
    stage(device.overlays, device.Overlays(here: "history", history_app: "so.tilo.notes"))
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
}

/// Ada's opens of an afternoon and two of Mia's, newest first, as the session lists them.
action recents_of_an_afternoon(d: Layout) {
    d.recents.insert(
        LayoutRecent(
            key: "notes:quotes",
            resource: "tl:so.tilo.notes:quotes",
            title: "Quotes for the release notes",
            origin_app: "so.tilo.notes",
            app_title: "Notes",
            kind: "so.tilo.notes.Note",
            by: "Ada Lovelace",
            at: Time("2026-11-23T16:21:00Z"),
            mine: true,
        ),
    )
    d.recents.insert(
        LayoutRecent(
            key: "tasks:release",
            resource: "tl:so.tilo.lang.tasks:release",
            title: "Release 4.2",
            origin_app: "so.tilo.lang.tasks",
            app_title: "Tasks",
            kind: "so.tilo.lang.tasks.List",
            by: "Ada Lovelace",
            at: Time("2026-11-23T15:47:00Z"),
            mine: true,
        ),
    )
    d.recents.insert(
        LayoutRecent(
            key: "notes:minutes",
            resource: "tl:so.tilo.notes:minutes",
            title: "Minutes of the Tuesday design review, with every decision and who owns it",
            origin_app: "so.tilo.notes",
            app_title: "Notes",
            kind: "so.tilo.notes.Note",
            by: "Ada Lovelace",
            at: Time("2026-11-23T11:02:00Z"),
            mine: true,
        ),
    )
    d.recents.insert(
        LayoutRecent(
            key: "notes:garden",
            resource: "tl:so.tilo.notes:garden",
            title: "Garden plan",
            origin_app: "so.tilo.notes",
            app_title: "Notes",
            kind: "so.tilo.notes.Note",
            by: "Mia Clayton",
            at: Time("2026-11-23T16:05:00Z"),
            mine: false,
        ),
    )
    d.recents.insert(
        LayoutRecent(
            key: "chat:launch",
            resource: "tl:so.tilo.lang.chat:launch",
            title: "Launch",
            origin_app: "so.tilo.lang.chat",
            app_title: "Chat",
            kind: "so.tilo.lang.chat.Conversation",
            by: "Mia Clayton",
            at: Time("2026-11-22T09:30:00Z"),
            mine: false,
        ),
    )
}
