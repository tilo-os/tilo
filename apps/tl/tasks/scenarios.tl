/// Scenarios: the app as people meet it, staged for the gallery, `tilolang layout-check` and the
/// tests. Each fills the workspace with one week of work and answers with the item its window
/// stands in, so a window opens where its story leaves the person. They are seed data for shots
/// and tests, never a person's: a real first launch starts empty.
///
/// A household and a small team in one week: an inbox to sort, two projects, some of it done.
scenario "a busy week" -> Task {
    let mia = person("Mia")
    let flat = tasks.projects.insert(
        Project(
            name: "Move flat",
            notes: RichText("Keys on the 15th. The van is booked for the morning."),
        ),
    )
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    tasks.items.insert(Task(title: "Take the bins out", done: true))
    let milk = tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    tasks.items.insert(
        Task(title: "Check what is in the fridge", parent: milk, due: 2026-06-30, done: true),
    )
    // What no one has sorted yet: no project, no day.
    tasks.items.insert(Task(title: "Sort the paper mail"))
    let call = tasks.items.insert(
        Task(
            title: "Call the landlord about the boiler",
            notes: RichText("Ask whether Tuesday morning works for the engineer."),
            due: 2026-06-30,
            at: 2026-06-30T09:00,
            priority: .high,
        ),
    )
    let service = tasks.items.insert(
        Task(title: "Find the last service date", parent: call, due: 2026-06-30),
    )
    tasks.items.insert(
        Task(title: "Look through the email folder", parent: service, due: 2026-06-30),
    )
    tasks.items.insert(
        Task(title: "Ask whether Tuesday morning works", parent: call, due: 2026-06-30),
    )
    tasks.items.insert(Task(title: "Book a haircut", due: 2026-06-29))
    tasks.items.insert(Task(title: "Reply to Mia about Sunday lunch", due: 2026-07-01))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, due: 2026-07-02))
    tasks.items.insert(
        Task(
            title: "Give notice to the electricity company",
            project: flat,
            due: 2026-07-04,
            priority: .medium,
        ),
    )
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Pack the books", project: flat, done: true))
    let copy = tasks.items.insert(
        Task(
            title: "Write the launch post",
            project: launch,
            due: 2026-07-06,
            priority: .urgent,
            notes: RichText("Lead with the offline editor, then sharing."),
            assignee: mia,
        ),
    )
    // Checked off before the subtask under it was added: clearing it keeps the subtask.
    let outline = tasks.items.insert(
        Task(title: "Outline", done: true, parent: copy, project: launch),
    )
    tasks.items.insert(Task(title: "Collect the beta quotes", parent: outline, project: launch))
    tasks.items.insert(Task(title: "First draft", parent: copy, project: launch))
    tasks.items.insert(Task(title: "Screenshots", parent: copy, project: launch))
    tasks.items.insert(
        Task(title: "Check the pricing page on phones", project: launch, due: 2026-07-08),
    )
    tasks.items.insert(Task(title: "Set up the redirects", project: launch, done: true))
    call
}

/// The week's log: what was finished on each day, newest first. It answers with one of them, whose
/// place is Done, so the window stands in Done with it open where it stands.
scenario "what got done this week" -> Task {
    let mia = person("Mia")
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(Task(title: "Book the van", project: flat, due: 2026-07-02))
    let boxes = tasks.items.insert(Task(title: "Order boxes and tape", project: flat))
    let notice = tasks.items.insert(
        Task(title: "Give notice to the electricity company", project: flat),
    )
    let passports = tasks.items.insert(Task(title: "Find the passports"))
    tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    invoke(check(passports, true), by: mia, at: 2026-06-23T12:00Z)
    invoke(check(van, true), by: mia, at: 2026-06-29T12:00Z)
    invoke(check(boxes, true), by: mia, at: 2026-06-29T15:40Z)
    invoke(check(notice, true), by: mia, at: 2026-06-30T07:15Z)
    passports
}

/// Today: what is due, what went by, and what is done with it.
scenario "today" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    tasks.items.insert(Task(title: "Buy oat milk and eggs", due: 2026-06-30))
    let call = tasks.items.insert(
        Task(
            title: "Call the landlord about the boiler",
            due: 2026-06-30,
            at: 2026-06-30T09:00,
            priority: .high,
        ),
    )
    tasks.items.insert(Task(title: "Book a haircut", due: 2026-06-29))
    tasks.items.insert(Task(title: "Pack the kitchen", project: flat, due: 2026-06-30))
    tasks.items.insert(Task(title: "Order boxes and tape", project: flat, due: 2026-07-02))
    // Mine, with no day of its own: it stands in my Today because it is mine.
    tasks.items.insert(Task(title: "Sort the paper mail", assignee: invoker))
    call
}

/// A launch post half written, and the subtasks it was broken into, one of them broken into its own.
scenario "a task's subtasks" -> Task {
    let mia = person("Mia")
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    let copy = tasks.items.insert(
        Task(
            title: "Write the launch post",
            project: launch,
            due: 2026-07-06,
            priority: .urgent,
            notes: RichText("Lead with the offline editor, then sharing."),
            assignee: mia,
        ),
    )
    // Checked off before the subtask under it was added: clearing it keeps the subtask.
    let outline = tasks.items.insert(
        Task(title: "Outline", done: true, parent: copy, project: launch),
    )
    tasks.items.insert(
        Task(title: "Collect the quotes from the beta testers", parent: outline, project: launch),
    )
    tasks.items.insert(
        Task(
            title: "First draft, with the quotes from the beta testers",
            parent: copy,
            project: launch,
        ),
    )
    tasks.items.insert(Task(title: "Screenshots", parent: copy, project: launch))
    copy
}

/// A week a person, a colleague and their agent share: who holds what, and what each did to it.
scenario "a shared week" -> Task {
    let mia = person("Mia")
    let scout = agent("Scout")
    let launch = tasks.projects.insert(Project(name: "Website launch"))
    let copy = tasks.items.insert(
        Task(title: "Write the launch post", project: launch, due: 2026-07-06, assignee: mia),
    )
    let quotes = tasks.items.insert(
        Task(
            title: "Collect the quotes from the beta testers",
            parent: copy,
            project: launch,
            assignee: scout,
        ),
    )
    tasks.items.insert(
        Task(
            title: "Check the pricing page on phones",
            project: launch,
            due: 2026-07-08,
            assignee: invoker,
        ),
    )
    tasks.items.insert(Task(title: "Book the van", due: 2026-07-10, assignee: mia))
    invoke(add_subtask(tasks, copy, "Ask three customers for a quote"), by: scout)
    invoke(check(quotes, true), by: mia)
    copy
}

/// A task added but not yet named: its row is the field its title is typed in.
scenario "a task being named" {
    tasks.items.insert(Task(title: "Book the van", due: 2026-07-10))
    tasks.items.insert(Task(title: ""))
}

/// A week everyone has finished: it answers with one of them, whose place is Done.
scenario "all done" -> Task {
    let passports = tasks.items.insert(Task(title: "Passports", done: true))
    tasks.items.insert(Task(title: "Chargers", done: true))
    tasks.items.insert(Task(title: "Sun cream", done: true))
    passports
}

/// A task the day went by on, with steps under it due the same day and later.
scenario "a task that is late" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let call = tasks.items.insert(
        Task(
            title: "Call the landlord about the boiler",
            notes: RichText("Ask whether Tuesday morning works for the engineer."),
            project: flat,
            due: 2026-06-29,
            at: 2026-06-29T09:00,
            priority: .high,
        ),
    )
    tasks.items.insert(
        Task(title: "Find the last service date", parent: call, project: flat, due: 2026-06-29),
    )
    tasks.items.insert(
        Task(title: "Book the engineer's visit", parent: call, project: flat, due: 2026-07-02),
    )
    call
}

/// A task whose notes name another one: its page draws the task it names as a card beside it.
scenario "a task naming another" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(
        Task(title: "Book the van", project: flat, due: 2026-07-10, priority: .high),
    )
    tasks.items.insert(
        Task(
            title: "Pack the kitchen",
            project: flat,
            notes: RichText("Put everything in ", van, " before the morning."),
        ),
    )
}

/// A task made from another, which was made from a third: its page names the one it was made from
/// and draws the task made from it as a card.
scenario "a task made from another" -> Task {
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(
        Task(title: "Book the van", project: flat, due: 2026-07-10, priority: .high),
    )
    let pack = invoke(
        tasks.items.insert(
            Task(title: "Pack the kitchen", project: flat, due: 2026-07-08, priority: .high),
        ),
        from: van,
    )
    invoke(
        tasks.items.insert(
            Task(title: "Load the boxes by the door", project: flat, due: 2026-07-09),
        ),
        from: pack,
    )
    pack
}

/// What an agent suggested about a task, before anyone decided it: its page says what the change
/// would be and who proposed it, and its row carries the same in a line. Nothing has changed yet.
scenario "an agent's suggestion" -> Task {
    let scout = agent("Scout")
    let flat = tasks.projects.insert(Project(name: "Move flat"))
    let van = tasks.items.insert(Task(title: "Book the van", project: flat))
    invoke(suggest schedule(van, due: 2026-07-10), by: scout)
    van
}

/// A move a week away, on its project's board: a lane per stage, the tasks in each.
scenario "a project board" -> Project {
    let flat = tasks.projects.insert(
        Project(name: "Move flat", notes: RichText("Keys on the 15th.")),
    )
    let packing = tasks.sections.insert(Section(name: "Packing", project: flat))
    let booked = tasks.sections.insert(Section(name: "Booked", project: flat))
    tasks.items.insert(Task(title: "Measure the new bedroom", project: flat))
    tasks.items.insert(Task(title: "Ask Mia about the sofa", project: flat, due: 2026-07-01))
    tasks.items.insert(Task(title: "Ring the letting agent", project: flat, due: 2026-06-29))
    tasks.items.insert(
        Task(
            title: "Order boxes and tape",
            project: flat,
            section: packing,
            due: 2026-07-02,
            priority: .medium,
        ),
    )
    tasks.items.insert(Task(title: "Pack the books", project: flat, section: packing, done: true))
    tasks.items.insert(
        Task(title: "Pack the kitchen", project: flat, section: packing, due: 2026-07-10),
    )
    tasks.items.insert(Task(title: "Book the van", project: flat, section: booked, priority: .high))
    tasks.items.insert(
        Task(
            title: "Give notice to the electricity company",
            project: flat,
            section: booked,
            due: 2026-07-04,
        ),
    )
    flat
}

/// A task page with the discussion under it: what two people said about the chairs, days apart.
scenario "a discussion on a task" -> Task {
    let ada = person("Ada")
    let bo = person("Bo")
    let t = tasks.items.insert(
        Task(
            title: "Order the new office chairs",
            notes: RichText("Two for the window desks, and four for the room."),
            due: 2026-07-01,
            assignee: ada,
        ),
    )
    invoke(
        discussion.comment(
            discussion.comments,
            RichText("The supplier quotes six weeks. Do we order before the summer?"),
        ),
        by: ada,
        at: 2026-06-24T09:12Z,
        from: t,
    )
    invoke(
        discussion.comment(discussion.comments, RichText("Yes. The room stays empty either way.")),
        by: bo,
        at: 2026-06-24T15:40Z,
        from: t,
    )
    invoke(
        discussion.comment(
            discussion.comments,
            RichText("Ordered, delivery the first week of August."),
        ),
        by: ada,
        at: 2026-06-25T08:03Z,
        from: t,
    )
    t
}

/// A day away from the week's tasks: while the person looking was out, a colleague and their agent
/// changed three of them, so the header counts what it holds unseen and the rows are marked.
scenario "while you were away" -> Task {
    let ada = invoker
    let bo = person("Bo")
    let scout = agent("Scout")
    let paper = Task(title: "Sort the paper mail")
    let permit = Task(title: "Ask the neighbours about the parking permit")
    let survey = Task(title: "Read the survey responses", due: 2026-07-02)
    let milk = Task(title: "Buy oat milk and eggs", due: 2026-06-30)
    paper = tasks.items.insert(paper)
    permit = tasks.items.insert(permit)
    survey = tasks.items.insert(survey)
    milk = tasks.items.insert(milk)
    invoke(assign(paper, ada), by: bo, at: 2026-06-29T16:20Z)
    invoke(schedule(survey, 2026-06-30), by: scout, at: 2026-06-29T17:05Z)
    invoke(check(milk, true), by: scout, at: 2026-06-29T17:12Z)
    survey
}

/// A task the person looking wrote, which a colleague changed since: its page says the change is
/// unseen, and its history says who made it.
scenario "a task that changed since you looked" -> Task {
    let bo = person("Bo")
    let call = Task(
        title: "Call the landlord about the boiler",
        notes: RichText("Ask whether Tuesday morning works for the engineer."),
        due: 2026-07-06,
        priority: .medium,
    )
    call = tasks.items.insert(call)
    invoke(schedule(call, 2026-06-30), by: bo, at: 2026-06-29T16:32Z)
    call
}
