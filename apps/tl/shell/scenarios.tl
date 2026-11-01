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
    d.notices.insert(Notice(key: "n7", title: "Screenshot ready", body: "The capture is in Pictures.", origin_app: "Capture Studio", group: "Capture Studio", kind: "info", read: true))
    d.notices.insert(Notice(key: "n8", title: "Ada replied in Chat", body: "Let's ship Friday.", origin_app: "Chat", group: "Chat", kind: "info", read: false))
    d.notices.insert(Notice(key: "n9", title: "Your laptop is at 12%", body: "Plug it in to finish the backup.", origin_app: "Power", group: "Power", kind: "warning", read: true))
    d.notices.insert(Notice(key: "n10", title: "Fieldwork filed 40 threads", body: "The mailbox is tidy.", origin_app: "Fieldwork", group: "Fieldwork", kind: "info", read: true))
    d.notices.insert(Notice(key: "n11", title: "New member in Studio", body: "Sam Okafor joined.", origin_app: "Studio", group: "Studio", kind: "info", read: true))
    d.notices.insert(Notice(key: "n12", title: "The release board moved to Review", body: "Jo moved 6 tasks.", origin_app: "Release 4.2", group: "Release 4.2", kind: "info", read: false))
    d.notices.insert(Notice(key: "n13", title: "Backup finished", body: "12 GB of changes are safe.", origin_app: "Settings", group: "Settings", kind: "info", read: true))
    d.notices.insert(Notice(key: "n14", title: "Calendar: Standup in 10 minutes", body: "Studio room.", origin_app: "Calendar", group: "Calendar", kind: "info", read: false))
    d.notices.insert(Notice(key: "n15", title: "Fieldwork is waiting for you", body: "It asks which folder to archive.", origin_app: "Fieldwork", group: "Fieldwork", kind: "warning", read: false, wants_reply: true))
    seed_notices(d)
    d.notice_actions.insert(NoticeAction(notice: "n1", key: "open", label: "Reply"))
    d.notice_actions.insert(NoticeAction(notice: "n1", key: "later", label: "Later"))
    d.notice_actions.insert(NoticeAction(notice: "n4", key: "open", label: "Open the run"))
    d.notice_actions.insert(NoticeAction(notice: "n4", key: "retry", label: "Retry"))
    d.notice_actions.insert(NoticeAction(notice: "n15", key: "open", label: "Answer"))
}

/// The centre with nothing new: every notice read, so its header says so and no row carries a
/// mark, and one group's choices with it.
scenario "the notification centre, all read" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "centre"
    d.unread = 0
    d.notices.insert(Notice(key: "n1", title: "Mia mentioned you in Release planning", body: "Can you check the launch date?", origin_app: "Notes", group: "Notes", kind: "info", read: true))
    d.notices.insert(Notice(key: "n2", title: "Three threads filed", body: "Fieldwork filed 3 threads into Receipts.", origin_app: "Fieldwork", group: "Fieldwork", kind: "info", read: true))
    d.notices.insert(Notice(key: "n3", title: "Release 4.2 moved to Review", body: "Jo moved the board on.", origin_app: "Tasks", group: "Tasks", kind: "info", read: true))
    d.notice_actions.insert(NoticeAction(notice: "n1", key: "open", label: "Reply"))
    d.notice_actions.insert(NoticeAction(notice: "n1", key: "later", label: "Later"))
}

/// The centre with nothing in it: what a person sees after Clear all.
scenario "an empty notification centre" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.workspace = "Studio"
    d.here = "centre"
    d.unread = 0
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

/// A conversation with the session's agent: what Ada asked, the agent's words between its
/// steps, a program it ran that printed two notes, one that changed a note, one of Ada's own
/// that did not compile, and the agent's answer.
scenario "a conversation with the agent" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "10:42 AM"
    d.workspace = "Studio"
    d.here = "conversation"
    d.runs.insert(Run(key: "r1", title: "Tidy the launch notes", agent: "claude", phase: "done", active: false))
    d.run_entries.insert(RunEntry(key: "r1/1", run: "r1", seq: 1, author: "ada", text: "Tidy the launch notes: pin the checklist and drop the old draft title."))
    d.run_entries.insert(RunEntry(key: "r1/2", run: "r1", seq: 2, author: "agent:claude", by_agent: true, text: "I will look for the launch notes first."))
    d.run_entries.insert(RunEntry(key: "r1/3", run: "r1", seq: 3, author: "agent:claude", by_agent: true, step: true, text: "find(notes.Note, \"launch\")", outcome: "done", answer: "n1 \"Launch checklist\"\nn2 \"Launch day (old draft)\""))
    d.entry_refs.insert(EntryRef(key: "r1/3/0", entry: "r1/3", resource: "tl:so.tilo.lang.notes:n1", app_id: "so.tilo.lang.notes", type_kind: "notes.Note", label: "Launch checklist"))
    d.entry_refs.insert(EntryRef(key: "r1/3/1", entry: "r1/3", resource: "tl:so.tilo.lang.notes:n2", app_id: "so.tilo.lang.notes", type_kind: "notes.Note", label: "Launch day (old draft)"))
    d.run_entries.insert(RunEntry(key: "r1/4", run: "r1", seq: 4, author: "agent:claude", by_agent: true, step: true, text: "notes.pin(n1)\nn2.title = \"Launch day\"", outcome: "done", answer: "n1 \"Launch checklist\" pinned\nn2 \"Launch day\""))
    d.entry_refs.insert(EntryRef(key: "r1/4/0", entry: "r1/4", changed: true, resource: "tl:so.tilo.lang.notes:n1", app_id: "so.tilo.lang.notes", type_kind: "notes.Note", label: "Launch checklist", change: "Pin"))
    d.entry_refs.insert(EntryRef(key: "r1/4/1", entry: "r1/4", changed: true, resource: "tl:so.tilo.lang.notes:n2", app_id: "so.tilo.lang.notes", type_kind: "notes.Note", label: "Launch day", change: "Rename"))
    d.run_entries.insert(RunEntry(key: "r1/5", run: "r1", seq: 5, author: "ada", step: true, text: "notes.Note.count(", outcome: "rejected", answer: "// line 1: expected an argument or `)` after `(`"))
    d.run_entries.insert(RunEntry(key: "r1/6", run: "r1", seq: 6, author: "agent:claude", by_agent: true, text: "The checklist is pinned and the old draft is called Launch day now."))
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

/// Adds a workspace, with one row, to `a`, and returns the row's key.
action a_workspace(a: Arrangement, name: Text) -> Text {
    let w = a.workspaces.insert(ArrangementWorkspace(key: "w-{name}", name: name))
    let r = a.rows.insert(ArrangementRow(key: "r-{name}", workspace: w.key, name: name, shell: "so.tilo.shell.strips"))
    r.key
}

/// Adds a strip named `name` to `row`, standing at `width`, and returns its key.
action a_strip(a: Arrangement, row: Text, name: Text, width: Int) -> Text {
    let s = a.strips.insert(ArrangementStrip(key: "s-{row}-{name}", workspace: "w-Desk", row: row, width: width, shell: "so.tilo.shell.strips"))
    s.key
}

/// Adds a place showing the note at `index` of the notebook to `strip`, and returns its key.
action a_note_place(a: Arrangement, strip: Text, index: Int) -> Text {
    let chosen = notes.notes.take(index + 1)
    if let n = chosen.last() {
        let p = a.places.insert(ArrangementPlace(key: "p-{index}", strip: strip, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        return p.key
    }
    ""
}

/// A place of a kind this build has no record for: it stands as it is, which is what a place
/// another shell made looks like (§2).
action a_foreign_place(a: Arrangement, strip: Text, index: Int, title: Text) -> Text {
    let p = a.places.insert(ArrangementPlace(key: "f-{index}", strip: strip, shows: "so.tilo.terminal", title: title, shell: "so.tilo.shell.other"))
    p.key
}

/// A working desk's places at the counts people keep: one place on its own, three across a
/// narrow list, a wide board and a usual note, then a busy day as three strips of four. The
/// strips stand side by side in one row, and the narrow list holds focus.
scenario "a desk of places" -> Arrangement {
    let v = viewing
    v.showing = .strips
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let one = a_strip(a, row, "One", 667)
    a_note_place(a, one, 0)
    let narrow = a_strip(a, row, "List", 333)
    a.places.insert(ArrangementPlace(key: "n-1", strip: narrow, shows: "so.tilo.lang.tasks", title: "Tasks", shell: "so.tilo.shell.strips"))
    let wide = a_strip(a, row, "Board", 1000)
    a_foreign_place(a, wide, 1, "A terminal")
    let usual = a_strip(a, row, "Note", 667)
    a_note_place(a, usual, 1)
    let day_a = a_strip(a, row, "Morning", 667)
    let day_b = a_strip(a, row, "Afternoon", 667)
    let day_c = a_strip(a, row, "Evening", 667)
    let i = 0
    for n in notes.notes.take(12) {
        let held = if i < 4 { day_a } else if i < 8 { day_b } else { day_c }
        a.places.insert(ArrangementPlace(key: "d-{i}", strip: held, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        i += 1
    }
    v.focused = narrow
    a
}

/// A strip with nothing on it yet, beside a strip with one place: the state a person starts
/// from when they make a strip.
scenario "an empty strip" -> Arrangement {
    let v = viewing
    v.showing = .strips
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    a_strip(a, row, "Empty", 667)
    let used = a_strip(a, row, "In use", 667)
    a_note_place(a, used, 0)
    a
}

/// A reference activated from a chat place: the channel's place, and the note's place it landed
/// beside — a column after the one it came from (shell.md §3).
scenario "a reference lands beside its place" -> Arrangement {
    let v = viewing
    v.showing = .strips
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let talks = a_strip(a, row, "Chat", 667)
    a.places.insert(ArrangementPlace(key: "ch", strip: talks, shows: "so.tilo.lang.chat", title: "design", shell: "so.tilo.shell.strips"))
    a_note_place(a, talks, 1)
    v.focused = talks
    a
}

/// Six places in two clusters on the framed canvas: one space per task, at three zoom levels as
/// the person looks closer or further out. Each note is its app's editor composed in its frame;
/// the terminal beside them is a window, shown in its well.
scenario "a canvas of spaces" -> Arrangement {
    let v = viewing
    v.showing = .frames
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
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
    for n in notes.notes.take(6) {
        let p = a.places.insert(ArrangementPlace(key: "c-{i}", strip: s, shows: "so.tilo.lang.notes", shown_as: "note_editor", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        i += 1
    }
    a.places.insert(ArrangementPlace(key: "c-terminal", strip: s, shows: "so.tilo.terminal", title: "A terminal", shell: "so.tilo.shell.strips"))
    a
}

/// The spaces canvas: the same places with no frame around them, so a resource's view stands as
/// content and nearness groups it. Each note is its app's editor composed in place; the terminal
/// beside them is a window, shown in its well.
scenario "resources standing as content" -> Arrangement {
    let v = viewing
    v.showing = .spaces
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let s = a_strip(a, row, "Spaces", 667)
    let strip = a.strips.find(st -> st.key == s)
    if let held = strip {
        held.canvas_x = 0
        held.canvas_y = 0
        held.canvas_width = 1100
        held.canvas_height = 1200
        held.placed = true
    }
    let i = 0
    for n in notes.notes.take(4) {
        a.places.insert(ArrangementPlace(key: "x-{i}", strip: s, shows: "so.tilo.lang.notes", shown_as: "note_editor", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
        i += 1
    }
    a.places.insert(ArrangementPlace(key: "x-terminal", strip: s, shows: "so.tilo.terminal", title: "A terminal", shell: "so.tilo.shell.strips"))
    a
}

/// The traditional desktop: the preset furthest from the strips and the canvas, staged with
/// real records. Three windows stand where the person left them on a floating desk — a note, the
/// note beside it and a chat channel — the note in front over the corners of the other two as a
/// person working leaves them, each still named by its own bar; the in-front place's own resource publishes the commands the top bar's
/// action strip draws (the notes app's `group:` runs), and the dock rides the foot. The desk
/// preset is the desktop and the chrome preset the traditional one, so this is exactly what a
/// person sees when they pick it.
scenario "the traditional desktop" {
    let d = root Desk
    d.person = "Ada Lovelace"
    d.clock = "4:28 PM"
    d.weekday = "Thursday"
    d.date = "October 30"
    d.unread = 2
    d.workspace = "Studio"
    d.strip = "Drafts"
    d.here = "none"
    d.workspaces.insert(Workspace(key: "home", name: "Home"))
    d.workspaces.insert(Workspace(key: "studio", name: "Studio", current: true))
    d.people.insert(Person(key: "mia", name: "Mia Clayton"))
    d.tasks.insert(Task(key: "sort-mail", title: "Sorting the inbox", state: "running"))
    d.windows.insert(Win(key: "w1", title: "Release planning", origin_app: "Notes", icon: "notebook", strip: "Desk", workspace: "Studio", recency: 1, focused: true, preview: "live"))
    d.windows.insert(Win(key: "w2", title: "Launch checklist", origin_app: "Notes", icon: "notebook", strip: "Desk", workspace: "Studio", recency: 2, focused: false, preview: "live"))
    d.windows.insert(Win(key: "w3", title: "design", origin_app: "Chat", icon: "message_square", strip: "Desk", workspace: "Studio", recency: 3, focused: false, preview: "live"))
    seed(shell)
    let s = shell
    s.dock_items.insert(DockItem(key: "notes", title: "Notes", icon: "file-text"))
    s.dock_items.insert(DockItem(key: "mail", title: "Mail", icon: "mail", badge: 3))
    s.dock_items.insert(DockItem(key: "chat", title: "Chat", icon: "message-square", running: true))
    s.dock_items.insert(DockItem(key: "files", title: "Files", icon: "folder"))
    apply(s, .windows_desktop)
    let pinned = notes.notes.filter(n -> n.pinned)
    let rest = notes.notes.filter(n -> not n.pinned)
    let note = pinned.first() ?? rest.first()
    let beside = pinned.last() ?? rest.last()
    let channel = channel_named("design")
    let v = viewing
    v.showing = .desktop
    let a = root Arrangement
    let row = a_workspace(a, "Desk")
    let desk = a_strip(a, row, "Desk", 667)
    if let held = a.strips.find(st -> st.key == desk) {
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
        a.places.insert(ArrangementPlace(key: "p1", strip: desk, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
    }
    if let n = beside {
        a.places.insert(ArrangementPlace(key: "p2", strip: behind, shows: "so.tilo.lang.notes", record: n.id(), title: n.title, shell: "so.tilo.shell.strips"))
    }
    if let c = channel {
        a.places.insert(ArrangementPlace(key: "p3", strip: talk, shows: "so.tilo.lang.chat", record: c.id(), title: c.name, shell: "so.tilo.shell.strips"))
    }
    if let strip = a.strips.find(st -> st.key == desk) {
        strip.active = "p1"
    }
    v.focused = desk
}

/// The text channel named `name` in the chat's sidebar, standing alone or in a category.
fn channel_named(name: Text) -> Option<chat.TextChannel> {
    for x in chat.channels {
        match x {
            c: chat.Category => {
                if let t = c.channels.find(t -> t.name == name) {
                    return .some(t)
                }
            }
            t: chat.TextChannel => {
                if t.name == name {
                    return .some(t)
                }
            }
            d: chat.Direct => {}
        }
    }
    .none
}
