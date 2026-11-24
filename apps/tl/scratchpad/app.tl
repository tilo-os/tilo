app so.tilo.scratchpad "Scratchpad" (
    icon: .file_text,
    category: .office,
    tagline: "Text you need only for a moment",
    keywords: ["scratch", "notepad", "quick note", "jot", "temporary", "sticky notes", "draft", "scribble", "notiz", "notizzettel", "schmierzettel", "schnellnotiz", "entwurf", "notizblock"],
)

/// The workspace's one note: what a person writes and nothing else. The app is this resource
/// and the editor over it; its history, sync and search are the system's, and it is named by
/// its first line, as a note begins.
type Note (untitled: "Untitled note") {
    body: RichText
}

root note: Note

default view note_page for Note

view note_page(n: Note) {
    std.document(n) {
        ui.BlockEditor(bind: n.body, placeholder: "Start writing…")
    }
}

/// A note for a Thursday.
scenario "a note" -> Note {
    let n = root Note
    n.body = RichText(
        "Notes for Thursday\n\nPick up the printed tickets before noon. Ask Maria whether the studio is free on the 14th, and send Tomas the revised floor plan.\n\nIdeas for the launch talk: start with the two-minute demo, keep the slides to a handful of images, and finish on the question the audience keeps asking.",
    )
    n
}

/// A note long enough to scroll on a laptop's screen.
scenario "a long note" -> Note {
    let n = root Note
    n.body = RichText(
        "Notes for Thursday\n\nPick up the printed tickets before noon. Ask Maria whether the studio is free on the 14th, and send Tomas the revised floor plan.\n\nIdeas for the launch talk: start with the two-minute demo, keep the slides to a handful of images, and finish on the question the audience keeps asking.\n\nWhat the room needs: two microphones that do not pick up the projector, a table by the door for the name tags, and water at every seat. The chairs stay in the horseshoe; nobody can see the screen from a straight row.\n\nWho does what on the day: Maria opens the doors at nine, Tomas runs the projector and keeps the spare bulb under the desk, and I hold the demo laptop and the backup on the stick in the front pocket of the bag.\n\nThe questions we keep getting: whether it works offline, what happens when two people type at once, and how a workspace is backed up. Answer the first with the two-minute demo, the second by opening two windows side by side, and the third in one sentence, without slides.\n\nAfterwards: send the recording to the list within the day, ask Maria to note who wanted the trial, and book the room again for the same hour a month from now.",
    )
    n
}
