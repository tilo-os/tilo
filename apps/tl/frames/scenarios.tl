/// What the shell shows for its own frames, as scenarios the layout check and screenshots
/// stage (shell.md §4).
///
/// A frame says what the resource and its type say, so these scenarios open this package's own
/// documents, taken from the notebook each scenario fills first: what a person reads in a
/// frame of a notes app's note is the same drawing, since every part of it is the resource's
/// and its type's.
/// A document the scenarios place in their frames: written into the notebook first, because a
/// place holds a resource the store has.
action make_document(title: Text, body: Text) -> Document {
    let n = root Arrangement
    n.documents.insert(Document(title: title, body: body))
}

/// One note open in a frame of its own.
scenario "one window" -> Arrangement {
    let a = root Arrangement
    let d = make_document("Quarterly plan", "What we ship this quarter, and what waits.")
    let strip = a.strips.insert(Strip(named: "Work"))
    let column = strip.stacks.insert(Stack())
    let frame = column.frames.insert(Frame(focused: true))
    let place = frame.places.insert(Place(shows: d, shows_as: "", window_state: ""))
    frame.front = .some(place)
    strip.front = .some(column)
    a.front = .some(strip)
    a
}

/// A frame with three tabs on a wide desk, the first in front.
scenario "three tabs" -> Arrangement {
    let a = root Arrangement
    let strip = a.strips.insert(Strip(named: "Work"))
    let column = strip.stacks.insert(Stack())
    let frame = column.frames.insert(Frame(focused: true))
    let one = make_document("Quarterly plan", "What we ship this quarter.")
    let two = make_document("Hiring loop", "Who we meet this week.")
    let three = make_document("Release notes", "What went out, and what people said.")
    let first = frame.places.insert(Place(shows: one, shows_as: "", window_state: ""))
    frame.places.insert(Place(shows: two, shows_as: "", window_state: ""))
    frame.places.insert(Place(shows: three, shows_as: "", window_state: ""))
    frame.front = .some(first)
    strip.front = .some(column)
    a.front = .some(strip)
    a
}

/// A frame with twelve tabs: the strip squeezes its pills to fit, and shows every title it can.
scenario "twelve tabs" -> Arrangement {
    let a = root Arrangement
    let strip = a.strips.insert(Strip(named: "Work"))
    let column = strip.stacks.insert(Stack())
    let frame = column.frames.insert(Frame(focused: false))
    let first = make_document("Quarterly plan", "One.")
    let first_place = frame.places.insert(Place(shows: first, shows_as: "", window_state: ""))
    let d_hiring = make_document("Hiring loop", "A page.")
    frame.places.insert(Place(shows: d_hiring, shows_as: "", window_state: ""))
    let d_release = make_document("Release notes", "A page.")
    frame.places.insert(Place(shows: d_release, shows_as: "", window_state: ""))
    let d_roadmap = make_document("Roadmap review", "A page.")
    frame.places.insert(Place(shows: d_roadmap, shows_as: "", window_state: ""))
    let d_design = make_document("Design critique", "A page.")
    frame.places.insert(Place(shows: d_design, shows_as: "", window_state: ""))
    let d_support = make_document("Support rotation", "A page.")
    frame.places.insert(Place(shows: d_support, shows_as: "", window_state: ""))
    let d_onboarding = make_document("Onboarding", "A page.")
    frame.places.insert(Place(shows: d_onboarding, shows_as: "", window_state: ""))
    let d_budget = make_document("Budget", "A page.")
    frame.places.insert(Place(shows: d_budget, shows_as: "", window_state: ""))
    let d_customer = make_document("Customer calls", "A page.")
    frame.places.insert(Place(shows: d_customer, shows_as: "", window_state: ""))
    let d_security = make_document("Security review", "A page.")
    frame.places.insert(Place(shows: d_security, shows_as: "", window_state: ""))
    let d_partner = make_document("Partner sync", "A page.")
    frame.places.insert(Place(shows: d_partner, shows_as: "", window_state: ""))
    let d_retro = make_document("Retro", "A page.")
    frame.places.insert(Place(shows: d_retro, shows_as: "", window_state: ""))
    frame.front = .some(first_place)
    strip.front = .some(column)
    a.front = .some(strip)
    a
}

/// Three frames stacked in one column: their heads stand above the expanded one, whose
/// content shows.
scenario "a stack of three" -> Arrangement {
    let a = root Arrangement
    let strip = a.strips.insert(Strip(named: "Work"))
    let column = strip.stacks.insert(Stack())
    let one = column.frames.insert(Frame())
    let two = column.frames.insert(Frame(focused: true))
    let three = column.frames.insert(Frame())
    let d1 = make_document("Quarterly plan", "The plan.")
    let d2 = make_document("Hiring loop", "The loop.")
    let d3 = make_document("Release notes", "The notes.")
    let first = one.places.insert(Place(shows: d1, shows_as: "", window_state: ""))
    one.front = .some(first)
    let second = two.places.insert(Place(shows: d2, shows_as: "", window_state: ""))
    two.front = .some(second)
    let third = three.places.insert(Place(shows: d3, shows_as: "", window_state: ""))
    three.front = .some(third)
    column.expanded = .some(two)
    strip.front = .some(column)
    a.front = .some(strip)
    a
}

/// Two columns side by side, each a frame of its own.
scenario "two columns" -> Arrangement {
    let a = root Arrangement
    let strip = a.strips.insert(Strip(named: "Work"))
    let left = strip.stacks.insert(Stack())
    let right = strip.stacks.insert(Stack())
    let list = left.frames.insert(Frame())
    let editor = right.frames.insert(Frame(focused: true))
    let inbox = make_document("Inbox", "The list.")
    let plan = make_document("Quarterly plan", "The document.")
    let notes = make_document("Release notes", "More.")
    let l = list.places.insert(Place(shows: inbox, shows_as: "", window_state: ""))
    list.front = .some(l)
    left.expanded = .some(list)
    let e = editor.places.insert(Place(shows: plan, shows_as: "", window_state: ""))
    let e2 = editor.places.insert(Place(shows: notes, shows_as: "", window_state: ""))
    editor.front = .some(e2)
    right.expanded = .some(editor)
    strip.front = .some(right)
    a.front = .some(strip)
    a
}
