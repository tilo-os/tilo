/// Scenarios: the desk as people meet it, in the states the shell's views are for. Each fills
/// the desk's own fields (the ones the session writes, which a scenario may set) and the
/// resources the session's origin carries, so every surface is staged as a screenshot and laid
/// out by `tilolang layout-check` with nothing running.
///
/// They are seed data for shots, tests and benchmarks, never a person's: a real first launch
/// starts empty.

/// An afternoon at a desk: three workspaces, four strips of the one in front with windows in
/// each, two people working alongside and a task running. The desk at rest.
scenario "an afternoon at the desk" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "4:28 PM"
    d.weekday = "Thursday"
    d.date = "October 30"
    d.unread = 3
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.title = "Studio — Drafts"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.workspaces.insert(Workspace(key: "release", name: "Release 4.2"))
    d.people.insert(Person(key: "mia", name: "Mia Clayton"))
    d.people.insert(Person(key: "sam", name: "Sam Okafor"))
    d.agents.insert(Agent(key: "fieldwork", name: "Fieldwork", state: "running", doing: "Sorting the inbox", mark: "3 threads filed", unseen: 3))
    d.tasks.insert(Task(key: "fieldwork", title: "Sorting the inbox", state: "running"))
    d.rooms.insert(Room(key: "studio", name: "Studio", current: true, members: "Ada, Mia, Sam, Fieldwork", present: 3, activity: "Mia edited Release planning"))
    d.rooms.insert(Room(key: "home", name: "Home", members: "Ada", present: 0, activity: "Your notes from the weekend"))
    d.rooms.insert(Room(key: "release", name: "Release 4.2", members: "Ada, Mia, Jo, Fieldwork", present: 2, activity: "The release board moved to Review"))
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.windows.insert(Win(key: "w2", title: "Launch checklist", origin_app: "Tasks", icon: "list_checks", strip: "Drafts", workspace: "Studio", recency: 2, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w3", title: "Screenshots for the docs", origin_app: "Gallery", icon: "layout_grid", strip: "Drafts", workspace: "Studio", recency: 3, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w4", title: "Interview notes", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 4, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w5", title: "Design review", origin_app: "Figma", icon: "pen_tool", strip: "Drafts", workspace: "Studio", recency: 5, focused: false, preview: "withheld"))
    d.windows.insert(Win(key: "w6", title: "Q3 budget", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 6, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w7", title: "Supplier quotes", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 7, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w8", title: "Pricing model", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 8, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w9", title: "Inbox", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 9, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w10", title: "From Ada", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 10, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w11", title: "Release notes draft", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 11, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w12", title: "Competitor scan", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 12, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w13", title: "Customer calls", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 13, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w14", title: "Type specimens", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 14, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w15", title: "Supplier quotes", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Home", recency: 15, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w16", title: "Release board", origin_app: "Tasks", icon: "list_checks", strip: "Drafts", workspace: "Release 4.2", recency: 16, focused: false, preview: "live"))
    seed_notices(d)
}

/// The overview of that desk: the strips of the workspace in front, their windows as places,
/// and a search typed across the other workspaces.
scenario "the overview, searching" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "4:31 PM"
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "overview"
    d.spotlight = "quotes"
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.people.insert(Person(key: "mia", name: "Mia Clayton"))
    d.agents.insert(Agent(key: "fieldwork", name: "Fieldwork", state: "running", doing: "Sorting the inbox", mark: "3 threads filed", unseen: 3))
    d.rooms.insert(Room(key: "studio", name: "Studio", current: true, members: "Ada, Mia, Fieldwork", present: 2, activity: "Mia edited Release planning"))
    d.rooms.insert(Room(key: "home", name: "Home", members: "Ada", present: 0, activity: "Your notes from the weekend"))
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.windows.insert(Win(key: "w2", title: "Launch checklist", origin_app: "Tasks", icon: "list_checks", strip: "Drafts", workspace: "Studio", recency: 2, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w3", title: "Screenshots for the docs", origin_app: "Gallery", icon: "layout_grid", strip: "Drafts", workspace: "Studio", recency: 3, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w6", title: "Q3 budget", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 6, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w9", title: "Inbox", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 9, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w15", title: "Supplier quotes", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Home", recency: 15, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w17", title: "Quotes for the printer", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Home", recency: 17, focused: false, preview: "live"))
}

/// The switcher with two windows: what Alt+Tab shows a moment after it is held.
scenario "the switcher, two windows" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "switcher"
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.windows.insert(Win(key: "w9", title: "Inbox", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 2, focused: false, preview: "live"))
}

/// The switcher with twenty windows: the walk does not fit one row.
scenario "the switcher, twenty windows" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "switcher"
    d.windows.insert(Win(key: "s1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.windows.insert(Win(key: "s2", title: "Launch checklist", origin_app: "Tasks", icon: "list_checks", strip: "Drafts", workspace: "Studio", recency: 2, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s3", title: "Screenshots for the docs", origin_app: "Gallery", icon: "layout_grid", strip: "Drafts", workspace: "Studio", recency: 3, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s4", title: "Interview notes", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 4, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s5", title: "Design review", origin_app: "Figma", icon: "pen_tool", strip: "Drafts", workspace: "Studio", recency: 5, focused: false, preview: "withheld"))
    d.windows.insert(Win(key: "s6", title: "Q3 budget", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 6, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s7", title: "Supplier quotes", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 7, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s8", title: "Pricing model", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 8, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s9", title: "Inbox", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 9, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s10", title: "From Ada", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 10, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s11", title: "Release notes draft", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 11, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s12", title: "Competitor scan", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 12, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s13", title: "Customer calls", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 13, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s14", title: "Type specimens", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 14, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s15", title: "Board minutes", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 15, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s16", title: "Onboarding script", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 16, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s17", title: "Weekly sync", origin_app: "Chat", icon: "message_circle", strip: "Mail", workspace: "Studio", recency: 17, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s18", title: "Architecture note", origin_app: "Notes", icon: "notebook", strip: "Research", workspace: "Studio", recency: 18, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s19", title: "Perf numbers", origin_app: "Charts", icon: "chart_line", strip: "Money", workspace: "Studio", recency: 19, focused: false, preview: "live"))
    d.windows.insert(Win(key: "s20", title: "Style guide", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 20, focused: false, preview: "live"))
}

/// The launcher an empty query shows: recent windows and the desk's doors.
scenario "the launcher, before typing" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "launcher"
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.windows.insert(Win(key: "w9", title: "Inbox", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 2, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w6", title: "Q3 budget", origin_app: "Sheets", icon: "table", strip: "Money", workspace: "Studio", recency: 3, focused: false, preview: "live"))
}

/// The launcher with "mai" typed: the windows that match.
scenario "the launcher, mail typed" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "launcher"
    d.spotlight = "mai"
    d.windows.insert(Win(key: "w9", title: "Inbox", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 1, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w10", title: "From Ada", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 2, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w11", title: "Mailbox rules", origin_app: "Mail", icon: "mail", strip: "Mail", workspace: "Studio", recency: 3, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 4, focused: false, preview: "live"))
}

/// A command palette query: an action, named the way people name it.
scenario "the launcher, an action typed" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "launcher"
    d.spotlight = "focus"
    d.windows.insert(Win(key: "w1", title: "Focus: the launch", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
}

/// The notification centre with fifteen notices from apps and from agent tasks.
scenario "the notification centre" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "centre"
    d.unread = 9
    d.notices.insert(Notice(key: "n7", title: "Screenshot ready", body: "The capture is in Pictures.", origin_app: "Capture Studio", group: "Capture Studio", kind: "success", read: true))
    d.notices.insert(Notice(key: "n8", title: "Ada replied in Chat", body: "Let's ship Friday.", origin_app: "Chat", group: "Chat", kind: "info", read: false))
    d.notices.insert(Notice(key: "n9", title: "Your laptop is at 12%", body: "Plug it in to finish the backup.", origin_app: "Power", group: "Power", kind: "warning", read: true))
    d.notices.insert(Notice(key: "n10", title: "Fieldwork filed 40 threads", body: "The mailbox is tidy.", origin_app: "Fieldwork", group: "Fieldwork", kind: "success", read: true))
    d.notices.insert(Notice(key: "n11", title: "New member in Studio", body: "Sam Okafor joined.", origin_app: "Studio", group: "Studio", kind: "info", read: true))
    d.notices.insert(Notice(key: "n12", title: "The release board moved to Review", body: "Jo moved 6 tasks.", origin_app: "Release 4.2", group: "Release 4.2", kind: "info", read: false))
    d.notices.insert(Notice(key: "n13", title: "Backup finished", body: "12 GB of changes are safe.", origin_app: "Settings", group: "Settings", kind: "success", read: true))
    d.notices.insert(Notice(key: "n14", title: "Calendar: Standup in 10 minutes", body: "Studio room.", origin_app: "Calendar", group: "Calendar", kind: "info", read: false))
    d.notices.insert(Notice(key: "n15", title: "Fieldwork is waiting for you", body: "It asks which folder to archive.", origin_app: "Fieldwork", group: "Fieldwork", kind: "warning", read: false, wants_reply: true))
    seed_notices(d)
    d.notice_actions.insert(NoticeAction(notice: "n1", key: "open", label: "Reply"))
    d.notice_actions.insert(NoticeAction(notice: "n1", key: "later", label: "Later"))
    d.notice_actions.insert(NoticeAction(notice: "n4", key: "open", label: "Open the run"))
    d.notice_actions.insert(NoticeAction(notice: "n4", key: "retry", label: "Retry"))
    d.notice_actions.insert(NoticeAction(notice: "n15", key: "open", label: "Answer"))
}

/// The composer with two tasks running and one done, and what they did.
scenario "the composer with tasks" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "composer"
    d.tasks.insert(Task(key: "t1", title: "Sorting the inbox", state: "running"))
    d.tasks.insert(Task(key: "t2", title: "Drafting the release notes", state: "running"))
    d.tasks.insert(Task(key: "t3", title: "Filed the receipts", state: "done"))
    d.agents.insert(Agent(key: "fieldwork", name: "Fieldwork", state: "running", doing: "Sorting the inbox", mark: "3 threads filed", unseen: 3))
    d.agents.insert(Agent(key: "scribe", name: "Scribe", state: "running", doing: "Drafting the release notes", mark: "4 paragraphs", unseen: 1))
    d.agents.insert(Agent(key: "clerk", name: "Clerk", state: "done", doing: "", mark: "12 receipts filed", unseen: 0))
    d.people.insert(Person(key: "mia", name: "Mia Clayton"))
}

/// The workspaces: every one the person is in, with who is there and what it holds.
scenario "the workspaces" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "9:12 AM"
    d.workspace = "Studio"
    d.here = "workspaces"
    d.rooms.insert(Room(key: "studio", name: "Studio", current: true, members: "Ada, Mia, Sam, Fieldwork", present: 3, activity: "Mia edited Release planning"))
    d.rooms.insert(Room(key: "home", name: "Home", members: "Ada", present: 0, activity: "Your notes from the weekend"))
    d.rooms.insert(Room(key: "release", name: "Release 4.2", members: "Ada, Mia, Jo, Fieldwork, two agents", present: 2, activity: "The release board moved to Review"))
    d.rooms.insert(Room(key: "garden", name: "Garden", members: "Ada, Jo", present: 1, activity: "Jo added a planting calendar"))
}

/// A banner over the desk: a build failure with its actions, above a desk at work.
scenario "a banner" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "2:04 PM"
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.rooms.insert(Room(key: "studio", name: "Studio", current: true, members: "Ada, Mia", present: 2, activity: "Mia edited Release planning"))
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Drafts", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.notices.insert(Notice(key: "n4", title: "Build failed", body: "The link check found 4 broken links.", origin_app: "Forgejo", group: "Forgejo", kind: "error", read: false))
    d.banner = "n4"
    d.notice_actions.insert(NoticeAction(notice: "n4", key: "open", label: "Open the run"))
    d.notice_actions.insert(NoticeAction(notice: "n4", key: "retry", label: "Retry"))
}

/// Who is here, and the menu a person's presence opens.
scenario "a person's menu" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.people.insert(Person(key: "mia", name: "Mia Clayton"))
    d.people.insert(Person(key: "sam", name: "Sam Okafor"))
    d.agents.insert(Agent(key: "fieldwork", name: "Fieldwork", state: "running", doing: "Sorting the inbox", mark: "3 threads filed", unseen: 3))
}

/// The desk's notices a scenario starts from: what apps and an agent said today.
action seed_notices(d: Desk) -> Int {
    d.notices.insert(Notice(key: "n1", title: "Mia mentioned you in Release planning", body: "Can you check the launch date?", origin_app: "Notes", group: "Notes", kind: "info", read: false, wants_reply: true))
    d.notices.insert(Notice(key: "n2", title: "Three threads filed", body: "Fieldwork filed 3 threads into Receipts.", origin_app: "Fieldwork", group: "Fieldwork", kind: "success", read: true))
    d.notices.insert(Notice(key: "n3", title: "Release 4.2 moved to Review", body: "Jo moved the board on.", origin_app: "Tasks", group: "Tasks", kind: "info", read: true))
    d.notices.insert(Notice(key: "n4", title: "Build failed", body: "The link check found 4 broken links.", origin_app: "Forgejo", group: "Forgejo", kind: "error", read: false))
    d.notices.insert(Notice(key: "n5", title: "Ada shared a notebook", body: "Launch checklist is now yours to edit.", origin_app: "Chat", group: "Chat", kind: "info", read: true))
    d.notices.insert(Notice(key: "n6", title: "Backup finished", body: "Your laptop backed up 12 GB of changes.", origin_app: "Settings", group: "Settings", kind: "success", read: true))
    return d.notices.len()
}
