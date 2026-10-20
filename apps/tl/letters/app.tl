app so.tilo.letters "Letters" (icon: .mail)

/// A letter: who it goes to, what it says, and whether it went.
type Draft {
    to: Text
    subject: Text (title)
    body: Text (body)
    sent: Bool = false
}

/// The workspace's letters.
type Desk {
    drafts: Collection<Draft>
}

/// Which letters the desk lists.
enum Shelf {
    /// Drafts
    drafts
    /// Sent
    sent
}

root Desk

default view desk for Desk

default view editor for Draft

row view draft_row for Draft

/// Starts a letter.
primary action new_draft(desk: Desk) -> Draft (icon: .plus, label: "New letter") {
    desk.drafts.insert(Draft(subject: "Untitled"))
}

/// Throws a letter away.
destructive action discard(desk: Desk, d: Draft) (icon: .trash) {
    desk.drafts.remove(d)
}

/// Posts the letter. This leaves the system, so it always asks first, showing the letter.
primary action send(d: Draft) proposal compose(d) (icon: .send) {
    if not d.sent {
        outside("post", (d.to, d.subject, d.body))
        d.sent = true
    }
}

/// The letter as it will leave, shown when someone is asked to send it.
view compose(d: Draft) {
    stack(gap: .related) {
        text("Send to {d.to}", role: .title)
        text(d.subject, role: .caption)
        text(d.body)
    }
}

/// The letters on a shelf.
fn shelved(desk: Desk, shelf: Shelf) -> List<Draft> {
    desk.drafts.filter(d -> d.sent == (shelf == .sent))
}

/// The desk's window: its letters, each opening in a window of its own. Which shelf it shows
/// is the window's.
view desk(desk: Desk) {
    window shelf: Shelf = .drafts
    list(shelved(desk, shelf), filter: shelf, empty: "No letters here")
}

/// One letter in a list: its subject and who it goes to.
view draft_row(d: Draft) {
    row(d.subject, secondary: d.to)
}

/// A letter's window.
view editor(d: Draft) {
    document {
        input(bind: d.to, placeholder: "To")
        input(bind: d.subject, placeholder: "Subject")
        input(bind: d.body, placeholder: "Write your letter", lines: 10)
        button(send(d), enabled: not d.sent) as send
    }
}

/// A desk with letters on both shelves: enough drafts to scroll.
scenario "a desk of letters" -> Draft {
    let desk = root Desk
    desk.drafts.insert(Draft(to: "bo@example.org", subject: "Dinner on Friday", body: "Shall we try the new place on Mill Lane? I can book for eight."))
    let boiler = desk.drafts.insert(Draft(to: "landlord@example.org", subject: "Boiler repair", body: "The boiler stopped again this morning. Could the engineer come on Tuesday?"))
    desk.drafts.insert(Draft(to: "mia@example.org", subject: "Photos from the trip", body: "Here are the ones from the lake. The sunset ones came out best."))
    desk.drafts.insert(Draft(to: "hr@example.org", subject: "Holiday in August", body: "I would like to take the second and third weeks of August off."))
    desk.drafts.insert(Draft(to: "sam@example.org", subject: "Happy birthday", body: "Thirty! Let me know when you are free for a proper celebration."))
    desk.drafts.insert(Draft(to: "council@example.org", subject: "Street light out", body: "The light outside number twelve has been out for a week."))
    desk.drafts.insert(Draft(to: "chen@example.org", subject: "Importer review", body: "I read through the table handling. Two small comments inline."))
    desk.drafts.insert(Draft(to: "library@example.org", subject: "Renewing two books", body: "Could I keep Piranesi and The Dispossessed for another three weeks?"))
    desk.drafts.insert(Draft(to: "dentist@example.org", subject: "Moving my appointment", body: "Could we move Thursday's check-up to the following week?"))
    desk.drafts.insert(Draft(to: "team@example.org", subject: "Notes from the offsite", body: "The three things we agreed on, and who is taking each one."))
    desk.drafts.insert(Draft(to: "editor@example.org", subject: "Second draft of the essay", body: "I cut the opening and moved the example about rivers to the end."))
    desk.drafts.insert(Draft(to: "neighbour@example.org", subject: "Parcel for you", body: "A parcel came for you while you were away. It is in our hallway."))
    desk.drafts.insert(Draft(to: "ada@example.org", subject: "Thanks for the help", body: "The sync fix worked. Coffee is on me next week.", sent: true))
    desk.drafts.insert(Draft(to: "bank@example.org", subject: "Change of address", body: "Please send post to the new flat from the first of the month.", sent: true))
    desk.drafts.insert(Draft(to: "gran@example.org", subject: "See you at Easter", body: "We will drive up on Thursday evening and stay until Monday.", sent: true))
    desk.drafts.insert(Draft(to: "club@example.org", subject: "Cancelling my membership", body: "I am moving away at the end of March, so please cancel from April.", sent: true))
    boiler
}

/// A first letter, half written.
scenario "a first letter" {
    let desk = root Desk
    desk.drafts.insert(Draft(to: "bo@example.org", subject: "Plans for the weekend", body: "Are you around on Saturday? I thought we could walk along the river."))
}
