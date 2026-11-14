app so.tilo.lang.fieldbox "Field box" (icon: .check, category: .office)

/// A task whose box edits its field and runs nothing else.
type Task {
    heading: Text
    checked: Bool
}

/// A page whose one choice is a field of its own: the box edits it and runs nothing else.
type Page {
    title: Text
    done: Bool
    tasks: Collection<Task>
}

root page: Page

/// Adds a task to the page's list.
action add(page: Page, title: Text) -> Task {
    return page.tasks.insert(Task(heading: title))
}

default view page_view for Page

view page_view(p: Page) {
    stack {
        ui.TextInput(bind: p.title, placeholder: "Title")
        checkbox("Done", bind: p.done)
        list(p.tasks) { t -> row(t.heading, checkbox: t.checked) }
    }
}
