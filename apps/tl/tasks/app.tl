app so.tilo.tasks "Tasks" (
    icon: .list_checks,
    category: .office,
    tagline: "Issues of the workspace, on a list and a board",
    keywords: ["tasks", "issues", "issue tracker", "kanban", "board", "to-do", "todo", "tickets", "projects", "agent tasks"],
    previously: [so.tilo.lang.todo, so.tilo.lang.tasks],
)

use so.tilo.lang.discussion as discussion

/// How much a task matters, most urgent last.
enum Priority {
    /// No priority
    none
    /// Low
    low
    /// Medium
    medium (icon: .flag, tone: .info)
    /// High
    high (icon: .flag, tone: .warning)
    /// Urgent
    urgent (icon: .triangle_alert, tone: .danger)
}

/// Something to do: what, by when, by whom, and in which project.
type Task {
    title: Text
    done: Bool = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// The moment on that day it is due at, if it has one.
    at: Option<Time> = .none
    /// When to tell whoever is doing it that its moment came, if a reminder is set.
    remind: Option<Time> = .none
    /// Anything worth remembering about it.
    notes: RichText
    /// Who is doing it.
    assignee: Option<Actor> = .none
    /// The project it belongs to; none while it waits in the inbox.
    project: Option<Project> = .none
    /// The list of its project's board it is in; none for the first list.
    section: Option<Section> = .none
    priority: Priority = .none
    /// The task it is a subtask of, to any depth; none for a task of its own.
    parent: Option<Task> = .none
    /// The words people sort it by across projects, such as "bug" or "errand".
    labels: Set<Text>
    /// What it waits for before anyone goes on with it, in a few words ("the landlord's reply");
    /// empty while it waits for nothing.
    waiting_for: Text = ""
    /// The latest moment its wait ends, whether or not what it waits for came.
    waiting_until: Option<Time> = .none
    /// The automation that ends its wait early when it runs, by its id: what an agent doing it
    /// waits on. Empty for a wait only its moment or a person ends.
    waiting_on: Text = ""

    search {
        filter "Inbox" = project == .none and due == .none and not done (icon: .inbox)
        // What waits for the person looking: what is due today or late, and what is theirs to do
        // whenever it is due, or due at no day at all.
        filter "Today" = not done and (assignee == workspace.me or due <= today) (icon: .star)
        filter "Upcoming" = due > today and not done (icon: .calendar)
        filter "Done" = done (icon: .circle_check)
        filter "Assigned to me" = assignee == workspace.me and not done (icon: .user)
        filter "Waiting" = waiting_for != "" and not done (icon: .hourglass)
        filter due
        filter project
        filter priority
        filter parent
        filter labels
        hide at
        hide section
        hide waiting_until
        hide waiting_on
    }
}

/// A project: tasks that work towards one outcome.
type Project {
    name: Text
    /// What it is for, and anything worth remembering about it.
    notes: RichText
}

/// A section of a project: one list of its board, such as "Doing".
type Section {
    name: Text
    /// The project it is a section of.
    project: Project
}

/// The workspace's tasks and projects.
type Tasks {
    /// Every task, in the order people put them.
    items: Collection<Task>
    projects: Collection<Project>
    /// The sections of every project, in the order its board shows them.
    sections: Collection<Section>
}

root tasks: Tasks

default view home for Tasks

default view task_page for Task

default view project_list for Project

views project_list (icon: .list), project_board (icon: .columns_2) for Project

row view task_summary for Task

place view to_do for Tasks

/// Adds a task to the inbox or to `p`, in the list `s` of its board: the entry at a list's end
/// fills `title` with the words nothing else there takes, and the day, moment and person they
/// name, so "Call the landlord fri 10am @Bo" is a task due Friday at ten, for Bo.
primary action new_task(
    tasks: Tasks,
    p: Option<Project> = .none,
    s: Option<Section> = .none,
    due: Option<Date> = .none,
    at: Option<Time> = .none,
    assignee: Option<Actor> = .none,
    title: Text = "",
    remind: Option<Time> = .none,
) -> Task (icon: .plus, label: "New task", key: "Ctrl+N") {
    tasks.items.insert(
        Task(
            title: title,
            due: due,
            at: at,
            remind: remind,
            assignee: assignee,
            project: p,
            section: s,
        ),
    )
}

/// The tracker the native Tasks app kept before this app took its id over, carried into this one
/// once. Each of its projects and areas is a project, and each issue a task with the id it had:
/// its title, its description, its priority, the day and moment it was due, who had it, its
/// labels, the project it worked towards, and the issue it stood under, in the tracker's order.
/// A finished or cancelled issue is a done task, and one in progress or in review stands in a
/// list of its project's board named so. What was said in an issue's thread is written under its
/// description, each comment with who said it and when. A sub-issue whose parent is gone stands
/// at the top level, as the native app showed it. The native app's own documents are left as
/// they are.
migration action carry_native_tracker(tasks: Tasks) uses time {
    let earlier = earlier(tasks)
    let projects: Map<Project> = Map<Project>()
    for area in earlier.filter(e -> e.of == "areas") {
        projects.put(
            area.id,
            tasks.projects.insert(Project(name: area.texts.get("name") ?? ""), id: area.id),
        )
    }
    // The tracker's order is each issue's rank, the same rank by id.
    let issues = earlier
        .filter(e -> e.of == "issues")
        .sort_by(e -> e.id)
        .sort_by(e -> e.texts.get("rank") ?? "")
    let held: Map<Text> = Map<Text>()
    for i in issues {
        held.put(i.id, i.id)
    }
    let parents: Map<Text> = Map<Text>()
    for i in issues {
        let p = i.texts.get("parent") ?? ""
        parents.put(i.id, if held.get(p) == .none { "" } else { p })
    }
    let steps = issues.len() + 1
    let ordered = [
        native_issues(issues, parents, ""),
        issues.filter(e -> issue_chain_end(parents, e.id, steps) == "circle"),
    ]
        .flat_map(x -> x)
    let lists: Map<Section> = Map<Section>()
    let carried: Map<Task> = Map<Task>()
    for i in ordered {
        let status = i.texts.get("status") ?? ""
        let area = i.texts.get("area.resource") ?? ""
        let project = projects.get(area)
        let list_name = if status == "in-progress" {
            "In progress"
        } else if status == "in-review" { "In review" } else { "" }
        let section: Option<Section> = if let p = project {
            if list_name == "" {
                .none
            } else if let s = lists.get("{area}/{status}") {
                s
            } else {
                let s = tasks.sections.insert(Section(name: list_name, project: p))
                lists.put("{area}/{status}", s)
                s
            }
        }
        let due: Option<Time> = if let ms = (i.texts.get("due_ms") ?? "").to_int() {
            Time("1970-01-01T00:00:00Z") + Duration(ms / 1000)
        }
        let at: Option<Time> = if let d = due { if d.time_of_day().seconds() > 0 { d } }
        // An issue whose parents run in a circle stands at the top, as the native list showed it.
        let parent = if issue_chain_end(parents, i.id, steps) == "circle" {
            ""
        } else { parents.get(i.id) ?? "" }
        let said = earlier
            .filter(e -> e.of == "issues/{i.id}/comments")
            .sort_by(c -> (c.texts.get("at_ms") ?? "").to_int() ?? 0)
            .map(c -> comment_by(c, member(member_key(c.texts))))
        let made = tasks.items.insert(
            Task(
                title: i.texts.get("title") ?? "",
                done: status == "done" or status == "cancelled",
                due: due?.day(),
                at: at,
                notes: issue_notes(i, said),
                assignee: member(i.texts.get("assignee") ?? ""),
                project: project,
                section: section,
                priority: native_priority(i.texts.get("priority") ?? ""),
                parent: if parent == "" { .none } else { carried.get(parent) },
            ),
            id: i.id,
        )
        for l in i.lists.get("labels") ?? [] {
            made.labels.add(l)
        }
        carried.put(i.id, made)
    }
}

/// What a native issue says under its title: its description, and under it what was said in its
/// thread, oldest first, each comment as [`comment_by`] reads it.
fn issue_notes(issue: host.Earlier, said: List<Text>) -> RichText {
    let body = issue.rich.get("body") ?? RichText("")
    if said.is_empty() {
        body
    } else {
        RichText.from_markdown(
            [[body.markdown, "", "**Said in its thread**"], said].flat_map(x -> x).join("\n\n"),
        )
    }
}

/// The key of the member a native comment names as its author, as `member` reads one: kept as
/// `{"person": "<key>"}` or `{"agent": "<id>"}`. Empty when it names no person or agent.
fn member_key(fields: Map<Text>) -> Text {
    if let p = fields.get("author.person") {
        p
    } else if let a = fields.get("author.agent") { "agent:{a}" } else { "" }
}

/// A line of a native comment's thread: who said it, and on which day, then what they said.
/// `who` is the member the comment named, when the workspace has them.
fn comment_by(c: host.Earlier, who: Option<Actor>) -> Text {
    let said_by = c.texts.get("author_name") ?? ""
    let name = if said_by == "" {
        if let m = who { m.name() } else { "Someone" }
    } else { said_by }
    let said = c.texts.get("text") ?? ""
    if let ms = (c.texts.get("at_ms") ?? "").to_int() {
        "{name}, {(Time("1970-01-01T00:00:00Z") + Duration(ms / 1000)).day()}: {said}"
    } else { "{name}: {said}" }
}

/// The priority a native issue kept as its level: 1 urgent to 4 low.
fn native_priority(level: Text) -> Priority {
    if level == "1" {
        .urgent
    } else if level == "2" {
        .high
    } else if level == "3" { .medium } else if level == "4" { .low } else { .none }
}

/// The native issues under `parent` (`""` for the top), each followed by the issues under it.
fn native_issues(
    issues: List<host.Earlier>,
    parents: Map<Text>,
    parent: Text,
) -> List<host.Earlier> {
    issues
        .filter(e -> (parents.get(e.id) ?? "") == parent)
        .flat_map(e -> [[e], native_issues(issues, parents, e.id)].flat_map(x -> x))
}

/// Where the chain of parents above the issue `id` ends, within `left` more steps: at the top
/// (`top`), or nowhere, going round (`circle`).
fn issue_chain_end(parents: Map<Text>, id: Text, left: Int) -> Text {
    let parent = parents.get(id) ?? ""
    if parent == "" {
        "top"
    } else if left <= 0 {
        "circle"
    } else {
        issue_chain_end(parents, parent, left - 1)
    }
}

/// Starts a project.
action new_project(tasks: Tasks) -> Project (icon: .folder_plus, label: "New project") {
    tasks.projects.insert(Project(name: std.say("New project")))
}

/// Checks a task off with every task under it, or opens it again with the tasks it is under.
action check(t: Task, done: Bool = not t.done) (
    label: if done { "Complete" } else { "Reopen" },
    icon: .check,
    on: t.done,
    key: "Ctrl+Enter",
) {
    t.done = done
    if done {
        for x in tasks.items.descendants(t, .parent) {
            x.done = true
        }
    } else {
        // Everything above the one opened is open again.
        for a in tasks.items.ancestors(t, .parent) {
            a.done = false
        }
    }
}

/// Adds a subtask under a task, in its project and section, and opens a done one: there is work
/// under it now.
action add_subtask(tasks: Tasks, t: Task, title: Text) -> Task uses time (
    label: "Add subtask",
    icon: .list_tree,
    did: "added a subtask",
    key: "Ctrl+Shift+N",
) {
    let made = tasks.items.insert(
        Task(title: title, parent: t, project: t.project, section: t.section),
    )
    let last = tasks.items.filter(x -> x.parent == t and x != made).last() ?? t
    tasks.items.move(made, after: last)
    if t.done or tasks.items.ancestors(t, .parent).any(a -> a.done) { check(t, false) }
    made
}

/// Sets the day a task is due by, from the presets or a day the person picks.
action due_today(t: Task) uses time (label: "Today", icon: .star, key: "Ctrl+T", group: "Due") {
    require not t.done "It is done already"
    t.at = moved(t.at, t.due, today)
    t.remind = moved(t.remind, t.due, today)
    t.due = today
}

/// Makes a task due tomorrow.
action due_tomorrow(t: Task) uses time (label: "Tomorrow", icon: .calendar, group: "Due") {
    require not t.done "It is done already"
    t.at = moved(t.at, t.due, today + 1d)
    t.remind = moved(t.remind, t.due, today + 1d)
    t.due = today + 1d
}

/// Makes a task due at the start of next week.
action due_next_week(t: Task) uses time (label: "Next week", icon: .calendar, group: "Due") {
    require not t.done "It is done already"
    t.at = moved(t.at, t.due, today.start_of_week() + 1w)
    t.remind = moved(t.remind, t.due, today.start_of_week() + 1w)
    t.due = today.start_of_week() + 1w
}

/// Makes a task due on a day the person picks. Its moment and its reminder move with it, at the
/// same time of day: a task due Wednesday at six, reminded at six, moved to Friday is due Friday
/// at six and reminded then.
action schedule(t: Task, due: Option<Date>) (
    label: "Pick date",
    icon: .calendar,
    key: "Ctrl+D",
    group: "Due",
) {
    t.at = moved(t.at, t.due, due)
    t.remind = moved(t.remind, t.due, due)
    t.due = due
}

/// `moment` moved with its task from the day `from` to the day `to`, at the same time of day; as
/// it was when either day is missing. Each action that moves a task's day writes it itself, so a
/// command over many chosen tasks stays one change.
fn moved(moment: Option<Time>, from: Option<Date>, to: Option<Date>) -> Option<Time> {
    if let m = moment {
        if let f = from { if let d = to { m + (d - f) } else { m } } else { m }
    }
}

/// Sets the time of day a task is due at: the day it is on stays, and a task with no day is due
/// on the day the moment names. A reminder that was set for the old moment moves with it, as a
/// day moved by `schedule` keeps its time of day: a task due Wednesday at six, reminded at six,
/// made seven is due at seven and reminded then.
action set_time(t: Task, at: Option<Time>) uses time (
    label: "Set time",
    icon: .clock,
    group: "Due",
) {
    let when: Option<Time> = if let m = at {
        if let d = t.due { .some(m + (d - m.day())) } else { .some(m) }
    } else { .none }
    if let old = t.at {
        if t.remind == .some(old) {
            t.remind = when
        }
    }
    t.at = when
    if t.due == .none {
        if let w = when {
            t.due = w.day()
        }
    }
}

/// Clears the day a task is due by: it waits in the inbox, undated.
action due_clear(t: Task) (label: "No date", group: "Due") {
    require t.due != .none "It has no date"
    t.due = .none
}

/// Rings a task's reminder when its moment comes, once for the whole workspace, window or not.
trigger ring(t: Task) at t.remind: remind(t)

/// Sets the moment whoever is doing a task is reminded of it. A reminder on a day after the task
/// is due moves the task to that day, its moment with it: being told of a task once its day has
/// gone is no reminder, so "move the reminder to Friday" moves both.
action remind_at(t: Task, when: Time) uses time (label: "Remind me", icon: .bell, group: "Remind") {
    if let d = t.due {
        if when.day() > d {
            t.at = moved(t.at, t.due, when.day())
            t.due = when.day()
        }
    }
    t.remind = when
}

/// Clears a task's reminder: it is not told of again.
action remind_clear(t: Task) uses time (label: "No reminder", icon: .bell_off, group: "Remind") {
    require t.remind != .none "It has no reminder"
    t.remind = .none
}

/// Tells whoever is doing a task, or the one who made it, what it is and when it is due: once,
/// and not of a task already done. Choosing the notice opens the task.
action remind(t: Task) uses notify {
    t.remind = .none
    if t.done {
        return
    }
    let who: Option<Actor> = if let a = t.assignee { a } else if let made = created(t) { made.who }
    if let w = who {
        notify(
            to: w,
            title: t.title,
            body: if let d = t.due { "Due {d}" } else if let p = t.project { p.name } else { "" },
            opens: t,
        )
    }
}

/// Sets how much a task matters.
action set_priority_none(t: Task) (label: "No priority", icon: .flag, group: "Priority") {
    require t.priority != .none "It has no priority"
    t.priority = .none
}

/// Marks a task as low priority.
action set_priority_low(t: Task) (label: "Low", icon: .flag, group: "Priority") {
    require t.priority != .low "It is low already"
    t.priority = .low
}

/// Marks a task as medium priority.
action set_priority_medium(t: Task) (label: "Medium", icon: .flag, group: "Priority") {
    require t.priority != .medium "It is medium already"
    t.priority = .medium
}

/// Marks a task as high priority.
action set_priority_high(t: Task) (label: "High", icon: .flag, group: "Priority") {
    require t.priority != .high "It is high already"
    t.priority = .high
}

/// Marks a task as urgent.
action set_priority_urgent(t: Task) (label: "Urgent", icon: .triangle_alert, group: "Priority") {
    require t.priority != .urgent "It is urgent already"
    t.priority = .urgent
}

/// Puts a task on hold until what it waits for comes, or until `until` at the latest: whoever
/// is doing it goes on with it then, a person or an agent alike. `on` names the automation that
/// ends the wait early when it runs.
action wait(t: Task, what: Text, until: Time, on: Text = "") uses time (
    label: "Wait for…",
    icon: .hourglass,
    did: "put on hold",
    group: "Plan",
) {
    require not t.done "It is done already"
    require not what.trim().is_empty() "Say what it waits for"
    require until > now "The wait ends in the past"
    t.waiting_for = what.trim()
    t.waiting_until = until
    t.waiting_on = on
}

/// Ends a task's wait: what it waited for came, and whoever is doing it goes on with it.
action resume(t: Task) (label: "Stop waiting", icon: .play, did: "went on with", group: "Plan") {
    applies t.waiting_for != "" "It waits for nothing"
    t.waiting_for = ""
    t.waiting_until = .none
    t.waiting_on = ""
}

/// Ends a task's wait when its moment comes, once for the whole workspace, window or not.
trigger wait_ends(t: Task) at t.waiting_until: resume(t)

/// Gives a task a label.
action label(t: Task, name: Text) (icon: .tag, label: "Label…", group: "Labels") {
    require not name.trim().is_empty() "Name the label"
    t.labels.add(name.trim())
}

/// Takes a label off a task.
action unlabel(t: Task, name: Text) (label: "Remove label", group: "Labels") {
    require t.labels.contains(name) "It has no such label"
    t.labels.remove(name)
}

/// Files a task under a project the person picks.
action file(t: Task, p: Project) (
    label: "Project",
    icon: .folder,
    key: "Ctrl+Shift+M",
    group: "Move to project",
) {
    t.project = p
}

/// Takes a task out of its project, back to the inbox.
action unfile(t: Task) (label: "Inbox", icon: .inbox, group: "Move to project") {
    require t.project != .none "It is in the inbox already"
    t.project = .none
}

/// Gives a task to someone, or to nobody.
action assign(t: Task, who: Option<Actor>) (label: "Assign", icon: .user) {
    t.assignee = who
}

/// Moves a task to a place in the outline: at the end of `parent`'s subtasks (the top level for
/// none), or where `before` stands. What is put under a done task opens it.
action move_to(tasks: Tasks, moved: Task, parent: Option<Task>, before: Option<Task>) uses time {
    require not within(tasks.items, parent, moved)
        and not within(tasks.items, before?.parent, moved) "A task cannot go inside itself"
    if let b = before {
        tasks.items.place(moved, .parent, before: b)
    } else {
        tasks.items.place(moved, .parent, inside: parent)
    }
    if not moved.done and tasks.items.ancestors(moved, .parent).any(a -> a.done) {
        check(moved, false)
    }
}

/// Moves a task under the task the view shows just over it in its group, as `Ctrl+]` does; with
/// none named, under the task above it in the list.
action indent(t: Task, above: Option<Task> (above)) uses time (
    label: "Indent",
    icon: .arrow_right,
    key: "Ctrl+]",
    group: "Move",
) {
    applies above != .none and above != t.parent "Nothing over it to move under"
    tasks.items.place(t, .parent, under: above)
    if not t.done and tasks.items.ancestors(t, .parent).any(a -> a.done) { check(t, false) }
}

/// Moves a task out from under its parent, just after it, as `Ctrl+[` does in an outline.
action outdent(t: Task) (label: "Outdent", icon: .arrow_left, key: "Ctrl+[", group: "Move") {
    applies t.parent != .none "It is at the top level already"
    if let p = t.parent { tasks.items.place(t, .parent, after: p) }
}

/// Copies a task, with every task under it, just after the one it copies.
action duplicate(tasks: Tasks, t: Task) -> Task (
    label: "Duplicate",
    icon: .copy,
    key: "Ctrl+Shift+D",
) {
    let under = tasks.items.descendants(t, .parent)
    let copy = tasks.items.insert(copied(t, t.parent))
    for l in t.labels {
        copy.labels.add(l)
    }
    if let last = last_below(tasks.items, t) {
        tasks.items.move(copy, after: last)
    } else { tasks.items.move(copy, after: t) }
    // Each copy hangs under the copy of the task it stands under, at the place it takes itself.
    for x in under {
        let under_copy = under.index_of(x.parent ?? t) ?? -1
        let at = tasks.items.descendants(copy, .parent).at(under_copy) ?? copy
        let below = last_below(tasks.items, at) ?? at
        let made = tasks.items.insert(copied(x, at))
        for l in x.labels {
            made.labels.add(l)
        }
        tasks.items.move(made, after: below)
    }
    copy
}

/// Deletes a task and its subtasks.
destructive action remove(tasks: Tasks, t: Task) (label: "Delete", icon: .trash, key: "Delete") {
    for x in tasks.items.descendants(t, .parent) {
        tasks.items.remove(x)
    }
    tasks.items.remove(t)
}

/// Deletes every finished task and answers how many went: a finished branch goes whole, and a
/// done task with a step still open under it stays. What was done is the list a person reads
/// back, so this is no command of the bar.
destructive action clear_done(tasks: Tasks) -> Int (
    icon: .circle_check,
    label: "Delete finished tasks",
) {
    // The guard is every task's own mark, read once: a done task stands in a finished branch.
    require tasks.items.count(x -> x.done) > 0 "Nothing is finished yet"
    let every = tasks.items.filter(x -> true)
    let gone = every.filter(x -> finished(every, x))
    for x in gone {
        tasks.items.remove(x)
    }
    gone.len()
}

/// Deletes a project and its sections; its tasks go back to the inbox.
destructive action remove_project(tasks: Tasks, p: Project) (
    icon: .trash,
    label: "Delete project",
) {
    for t in tasks.items.filter(t -> t.project == p) {
        t.project = .none
        t.section = .none
    }
    tasks.sections.remove_where(s -> s.project == p)
    tasks.projects.remove(p)
}

/// Adds a section at the end of a project's board.
action new_section(p: Project) -> Section (icon: .plus, label: "New section") {
    tasks.sections.insert(Section(name: std.say("New section"), project: p))
}

/// Renames a section.
action rename_section(s: Section, name: Text) {
    s.name = name
}

/// Deletes a section; its tasks go to the project's first list.
destructive action remove_section(s: Section) (icon: .trash, label: "Delete section") {
    for t in tasks.items.filter(t -> t.section == s) {
        t.section = .none
    }
    tasks.sections.remove(s)
}

/// Adds a task to a project, at the end of the list `s` of its board (none for the first). It is the
/// project's primary action, so its key brings the person to the entry that adds there.
primary action add_to(p: Project, title: Text, s: Option<Section> = .none) -> Task (
    icon: .plus,
    label: "New task",
    key: "Ctrl+N",
) {
    tasks.items.insert(Task(title: title, project: p, section: s))
}

/// Puts a task into the list `s` of its project's board (none for the first) at the place `at`
/// among the tasks already there: what a card's drag names.
action place(t: Task, s: Option<Section>, at: Int) (label: "Place in board") {
    require s == .none or s?.project == t.project "That list belongs to another project"
    applies t.project != .none "It is in no project's board"
    t.section = s
    tasks.items.move(
        t,
        to: at,
        among: tasks.items.filter(x -> x.project == t.project and x.section == s and not x.done),
    )
}

/// Renames a project.
action rename(p: Project, name: Text) {
    p.name = name
}

/// Whether `place` is `top` or one of the tasks over it.
fn within(items: List<Task>, place: Option<Task>, top: Task) -> Bool {
    if let p = place {
        p == top or items.ancestors(p, .parent).any(a -> a == top)
    } else { false }
}

/// `t` as a task of its own under `parent`: what a copy of it says, without the tasks under it
/// and its labels, which the copy is given once it is made.
fn copied(t: Task, parent: Option<Task>) -> Task {
    Task(
        title: t.title,
        done: t.done,
        due: t.due,
        at: t.at,
        notes: t.notes,
        assignee: t.assignee,
        project: t.project,
        section: t.section,
        priority: t.priority,
        parent: parent,
    )
}

/// The task that ends the run `t` begins: the last task under `t`, or `t` itself.
fn last_below(items: List<Task>, t: Task) -> Option<Task> {
    items.filter(x -> x == t or items.ancestors(x, .parent).any(a -> a == t)).last()
}

/// Whether `t` is finished: done itself, and every task under it, read over the caller's list.
fn finished(items: List<Task>, t: Task) -> Bool {
    let under = items.descendants(t, .parent)
    if under.is_empty() { t.done } else { under.all(x -> x.done) }
}

/// Whether `t` stands in a list's Done tail: finished, and everything over it too.
fn folded(items: List<Task>, t: Task) -> Bool {
    finished(items, t) and items.ancestors(t, .parent).all(a -> finished(items, a))
}

/// How many of a task's subtasks are done ("1/3"), wherever they stand: its own column of the
/// index, so a change to any other task leaves it as it was. None before the whole workspace is
/// read.
fn subtask_count(t: Task) -> Option<Text> {
    let mine = tasks.items.filter(x -> x.parent == t)
    if let total = mine.counted() {
        if not mine.is_empty() { "{mine.count(x -> x.done)}/{total}" }
    }
}

/// When a task is due of its own: a subtask repeats no day its parent already says.
fn own_due(t: Task, under: Option<Task>) -> Option<Date> {
    if let p = under { if p.due != t.due { t.due } } else { t.due }
}

/// When a task was finished: when its box was last checked off, from the index, never its history.
fn finished_at(t: Task) -> Option<Time> {
    if let v = changed(t, .done) { if t.done { v.at } }
}

/// The day a task stands under in a place's list: its due day, or in Done the day it was finished.
fn task_day(t: Task, by_day: Bool, done_place: Bool) -> Option<Date> {
    if by_day { t.due } else if done_place { finished_at(t)?.day() }
}

/// The open tasks of one list of a board: those of section `s`, or of none; never a card.
fn list_of(p: Project, s: Option<Section>) -> List<Task> {
    tasks.items.filter(t -> t.project == p and t.section == s and not t.done and t.parent == .none)
}

/// How many wait somewhere, as a badge shows it: none while nothing waits, or before all is read.
fn waiting(count: Option<Int>) -> Option<Int> {
    if let n = count { if n > 0 { count } }
}

/// The places a window offers: what is assigned to me is one only where someone else may share it.
fn places(shared: Bool) -> List<Filter<Task>> {
    if shared {
        [Task.inbox, Task.today, Task.upcoming, Task.done, Task.assigned_to_me]
    } else {
        [Task.inbox, Task.today, Task.upcoming, Task.done]
    }
}

/// The place a task stands in: its project's, or the first of `side` that holds it, else Inbox.
fn place_of(side: List<Filter<Task>>, x: Task) -> Filter<Task> {
    if let p = x.project {
        Task.project(p)
    } else {
        side.find(f -> tasks.items.matching(f).any(t -> t == x)) ?? Task.inbox
    }
}

/// The tasks window: Inbox, Today, Upcoming, Done, what is assigned to me, and the projects.
view home(t: Tasks) {
    state place: Filter<Task> = Task.today
    state opened: Option<Task> = .none
    // What the bar's search holds: it narrows the place's tasks beside the place chosen.
    state searched: Text = ""
    on show(x: Task): set(place, place_of(places(workspace.members.len() > 1), x))
        .then(set(opened, x))
    let side = places(workspace.members.len() > 1)
    let chosen = side.find(f -> f == place)
    let listed = t.items.matching(place)
    let found = if searched.is_empty() { listed } else { listed.matching(searched) }
    let project = t.projects.find(p -> place == Task.project(p))
    // A place of days groups by day and a project's list folds finished work under its foot.
    let by_day = place == Task.today or place == Task.upcoming or project != .none
    let done_place = place == Task.done
    let folds = project != .none
    let in_today = place == Task.today
    // Due soonest first, Done newest finished first, a project's list with finished work last.
    let ordered = found.sort_by(.due)
    let shown = if done_place {
        found.sort_by(x -> changed(x, .done)?.at, desc)
    } else if folds { ordered.sort_by(x -> folded(found, x)) } else { ordered }
    let title = if let p = project { p.name } else { chosen?.label ?? "Tasks" }
    // What waits here, read from the index: the same cost at three tasks or three thousand.
    let open: Int = if let p = project {
        found.len() - t.items.matching(Task.project(p), Task.done).len()
    } else { found.len() }
    let all_done = open == 0 and not found.is_empty()
    // The places whose badge counts what waits: what a person goes to, once the whole is read.
    let counted: List<Text> = ["inbox", "today", "assigned_to_me"]
    let header_count = if let c = chosen {
        if counted.any(k -> k == c.key) { waiting(found.counted()) }
    }
    let entry_placeholder = if let p = project {
        "New task in {p.name}"
    } else if place == Task.today { "New task for today" } else { "New task" }
    ui.NavShell(
        content_kind: .list,
        sidebar_title: "Tasks",
        title: title,
        count: header_count,
        actions: [new_task, clear_done],
    ) {
        on search(q: Text): set(searched, q)
        sidebar: {
            ui.NavList(selected: place) {
                for f in side {
                    let badge = if counted.any(k -> k == f.key) {
                        waiting(t.items.matching(f).counted())
                    }
                    ui.NavItem(f, f.label, icon: f.icon, count: badge) {
                        // Choosing a place is leaving the task that was open where it stood: it
                        // is another list, and a task of the one left does not belong in it.
                        on select: set(place, f).then(set(opened, .none))
                    }
                }
                ui.NavSection("Projects", add: new_project(t)) {
                    for p in t.projects {
                        ui.NavItem(Task.project(p), p.name, icon: .folder) {
                            on select: set(place, Task.project(p)).then(set(opened, .none))
                        }
                    }
                }
            }
        }
        // The place's tasks: the list is the column's own scroller, so the window lays out the
        // rows it shows whatever the workspace holds, and a task opens where it stands under its
        // row, in front of the list in a window too narrow to hold one beside the other. The
        // entry that adds to them is docked at the window's foot.
        content: {
            ui.Docked {
                foot: {
                    stack {
                        if all_done {
                            task_list(shown, by_day, done_place, folds, opened, project)
                        }
                        // A task typed in Today that names no day is due today, so it stands
                        // where it was typed.
                        let add = new_task(t, p: project, due: if in_today { today })
                        std.add_entry(entry_placeholder, add)
                    }
                }
                if all_done {
                    ui.EmptyState(phrase("tilolang-all-done"), icon: .list_checks, inline: true)
                } else if shown.is_empty() and opened == .none {
                    // A task open in the list stands in it after it leaves the place, so the
                    // list stays while one is open.
                    ui.EmptyState("No tasks here", icon: .list_checks)
                } else {
                    task_list(shown, by_day, done_place, folds, opened, project)
                }
            }
        }
    }
}

/// What there is to do, on its own as a place shows it: the open tasks, due soonest
/// first and grouped by day, each opening where it stands, with the entry that adds one at their
/// foot. None of the window's places, search or bar is around them.
view to_do(t: Tasks) {
    state opened: Option<Task> = .none
    on show(x: Task): set(opened, x)
    let open = t.items.filter(x -> not x.done).sort_by(.due)
    ui.Docked {
        foot: {
            stack {
                std.add_entry("New task", new_task(t))
            }
        }
        if open.is_empty() {
            ui.EmptyState("Nothing to do", icon: .list_checks)
        } else {
            task_list(open, true, false, false, opened, .none)
        }
    }
}

/// The tasks of a place as a list, branches nested under their tasks, finished work under the foot.
view task_list(
    items: List<Task>,
    by_day: Bool,
    done_place: Bool,
    folds: Bool,
    opened: State<Option<Task>>,
    project: Option<Project>,
) {
    std.list(
        items,
        opened,
        tree: true,
        ungrouped: if by_day { phrase("tilolang-no-date") },
        tail_title: if folds { phrase("tilolang-done") },
    ) { (x, at) ->
        let heading = task_day(x, by_day, done_place)
        let tail = folds and folded(items, x)
        // The task just over this one in its group, on this side of the fold: what `indent`
        // moves under, and none for the first of a group.
        let above = if let p = items.previous(x) {
            if task_day(p, by_day, done_place) == heading and (folds and folded(items, p)) == tail {
                p
            }
        }
        task_item(x, at, project, heading, done_place, tail, above, subtask_count(x))
    }
}

/// One task as a row of a list, and the document that opens into it where it stands.
view task_item(
    t: Task,
    at: Option<Actor>,
    project: Option<Project>,
    group: Option<Date>,
    group_done: Bool,
    tail: Bool,
    above: Option<Task>,
    progress: Option<Text>,
) {
    ui.ListItem(
        item: t,
        presence: at,
        group: group,
        group_done: group_done,
        done: t.done,
        tail: tail,
        above: above,
        parent: t.parent,
        parent_title: t.parent?.title,
        progress: progress,
        actions: [
            check(primary: true),
            schedule(group: "Plan"),
            assign,
            label(group: "Plan"),
            wait(group: "Plan"),
            resume(group: "Plan"),
            indent(group: "Move"),
            outdent(group: "Move"),
            remove,
        ],
    ) {
        // The row leaves out the project the window is of, and the day its group is headed by.
        task_row(t, project, group != .none, t.parent)
        detail: {
            task_body(t)
        }
        menu: {
            ui.MenuItem("Open as page", icon: .arrow_up_right) {
                on choose: open(t, view: task_page)
            }
        }
    }
}

/// A project's window: its tasks, with the entry that adds to them at their foot.
view project_list(p: Project) {
    state opened: Option<Task> = .none
    on show(x: Task): set(opened, x)
    let listed = tasks.items.filter(t -> t.project == p)
    ui.NavShell(content_kind: .list, title: p.name) {
        content: {
            // The project's tasks as the column's own scroller, the entry that adds to them
            // docked at the window's foot: a task opens where it stands under its row, in front
            // of the list where the window has not the room.
            ui.Docked {
                foot: {
                    stack {
                        std.add_entry("New task in {p.name}", new_task(tasks, p: p))
                    }
                }
                if listed.is_empty() {
                    ui.EmptyState("No tasks yet", icon: .folder)
                } else {
                    task_list(listed, false, false, false, opened, .some(p))
                }
            }
        }
    }
}

/// A project's board: a list per section, the first for the tasks in none, each open task a card.
view project_board(p: Project) {
    let sections = tasks.sections.filter(s -> s.project == p)
    std.page(p, canvas: true, fill: true) {
        ui.Scroll(across: true) {
            ui.Board(name: p.name) {
                let loose = list_of(p, .none)
                // No section named: it keeps its heading's height so cards line up, and is not headed.
                ui.BoardList {
                    on drop(moved: Task, at: Int): place(moved, .none, at)
                    foot: {
                        std.add_entry("New task", new_task(tasks, p: p))
                    }
                    for t in loose {
                        task_card(t, p, sections)
                    }
                }
                for s in sections {
                    let cards = list_of(p, s)
                    ui.BoardList(s, title: s.name, count: cards.counted()) {
                        on drop(moved: Task, at: Int): place(moved, s, at)
                        foot: {
                            std.add_entry("New task", new_task(tasks, p: p, s: s))
                        }
                        for t in cards {
                            task_card(t, p, sections)
                        }
                    }
                }
            }
        }
    }
}

/// A task on a board: its row, opening in a window of its own, with its list moves in its menu.
view task_card(t: Task, p: Project, sections: List<Section>) {
    ui.BoardCard(t, actions: [check(primary: true), schedule(group: "Plan"), assign, remove]) {
        on activate: open(t)
        menu: {
            if t.section != .none {
                ui.MenuItem("Remove from section", icon: .arrow_right) {
                    on choose: place(t, .none, 0)
                }
            }
            for s in sections {
                if t.section != s {
                    ui.MenuItem("Move to {s.name}", icon: .arrow_right) {
                        on choose: place(t, s, 0)
                    }
                }
            }
        }
        task_row(t, .some(p), false, .none)
    }
}

/// One task as a row, leaving out what the window and the row over it already say: the project
/// the window is of (`none` for a place that gathers the tasks of every project), and the day
/// the row's group is headed by.
view task_row(t: Task, project: Option<Project>, day: Bool, under: Option<Task>) {
    let place = if t.project != project { t.project?.name }
    let due: Option<Date> = if day { .none } else { own_due(t, under) }
    row(
        t.title,
        checkbox: t.done,
        check: check(t),
        secondary: place,
        note: proposal_note(t),
        meta: due,
        mark: t.priority,
        avatar: t.assignee,
    )
}

/// A task as the system shows it on a surface of its own — a result, a notice — with every fact.
view task_summary(t: Task) {
    row(
        t.title,
        checkbox: t.done,
        check: check(t),
        secondary: t.project?.name,
        meta: t.due,
        mark: t.priority,
        avatar: t.assignee,
    )
}

/// One task in an outline, counting done subtasks while folded, naming only what differs.
view task_outline_row(t: Task, project: Option<Project>, under: Option<Task>) {
    ui.TreeViewRow(
        checkbox: t.done,
        icon: t.priority.icon(),
        secondary: if t.project != project { t.project?.name },
        trailing: own_due(t, under),
    )
}

/// A task as a page of its own, outside the list it stands in.
view task_page(t: Task) {
    std.document(t) {
        task_body(t)
    }
}

/// A task where it stands: opened in the list it is an item of, or as a page of its own.
template view task_body(t: Task) {
    state step: Text
    // The task's head — what it is and what was written of it — and the groups under it: the
    // page's own blocks, a group gutter apart.
    stack(gap: .group) {
        stack(gap: .related) {
            row(gap: .related, align: .start) {
                ui.Checkbox(bind: t.done) {
                    on check: check(t)
                }
                ui.TextInput(bind: t.title, placeholder: "What to do", title: true, fill: 1.0)
            }
            if t.waiting_for != "" {
                row(gap: .related, align: .center) {
                    ui.Icon(.hourglass, size: .small)
                    ui.Text(
                        if let u = t.waiting_until {
                            "Waiting for {t.waiting_for}, until {u}"
                        } else { "Waiting for {t.waiting_for}" },
                        secondary: true,
                    )
                    ui.Button("Stop waiting", icon: .play) {
                        on press: resume(t)
                    }
                }
            }
            if not t.labels.is_empty() {
                row(gap: .related, wrap: true) {
                    for l in t.labels {
                        ui.Chip(l) {
                            on remove: unlabel(t, l)
                        }
                    }
                }
            }
            stack(lead: 1) {
                std.origin(t)
                ui.BlockEditor(bind: t.notes, placeholder: "Anything worth remembering about it.")
            }
        }
        std.body() {
            std.suggestions(t)
            facts(t) {
                ui.MenuButton(t.project?.name ?? "Inbox", icon: .folder) {
                    ui.MenuItem("Inbox", icon: .inbox, checked: t.project == .none) {
                        on choose: unfile(t)
                    }
                    for p in tasks.projects {
                        ui.MenuItem(p.name, icon: .folder, checked: t.project == p) {
                            on choose: file(t, p)
                        }
                    }
                }
                ui.MenuButton(
                    if t.remind == .none { "Remind" } else { "Reminder set" },
                    icon: .bell,
                ) {
                    ui.MenuItem("Remind me…", icon: .bell) {
                        on choose: remind_at(t)
                    }
                    ui.MenuItem("No reminder", icon: .bell_off, checked: t.remind == .none) {
                        on choose: remind_clear(t)
                    }
                }
            }
            section("Subtasks") {
                // The walk over the tasks under this one is made only where there is one.
                if not tasks.items.filter(x -> x.parent == t).is_empty() {
                    let kids = tasks.items.descendants(t, .parent)
                    std.outline(
                        "Subtasks",
                        kids,
                        x -> x.parent,
                        x -> x.title,
                        "No subtasks yet",
                        (moved, parent, before) -> move_to(
                            tasks,
                            moved,
                            parent,
                            before,
                        ),
                        x -> check(x),
                        progress: x -> subtask_count(x),
                        fill: false,
                    ) { (x, under) ->
                        task_outline_row(x, t.project, under)
                    }
                }
                ui.TextInput(bind: step, placeholder: "Add a subtask") {
                    on submit: add_subtask(tasks, t, step).then(set(step, ""))
                }
            }
            std.after_body {
                std.linked(t, without: discussion.said_about(t))
                discussion.thread(t)
                std.history(t)
            }
        }
    }
}
