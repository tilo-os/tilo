/// A gallery that has never been opened opens filled: the platform applies this once, on the
/// gallery's first start, as it applies every migration.
migration action seed(g: Gallery) {
    show_examples(g)
}

/// What the gallery shows the day it is opened with nothing in it: one document of every group,
/// the same shape the scenarios stage for a check, so a person meets the components rather than
/// an empty window. Each group's page reads its document from the root.
action show_examples(g: Gallery) {
    if g.basics.is_empty() {
        g.basics.insert(Basics(title: "Basics", owner: invoker, updated: 2026-06-30T09:00Z))
    }
    if g.controls.is_empty() {
        g.controls.insert(
            Controls(
                title: "Controls",
                starts: 2026-11-03,
                clock: 2026-11-03T09:30Z,
                owner: invoker,
            ),
        )
    }
    if g.feedback.is_empty() {
        g.feedback.insert(Feedback(title: "Feedback", owner: invoker))
    }
    if g.backlogs.is_empty() {
        let b = g.backlogs.insert(Backlog(title: "Backlog"))
        b.items.insert(
            Item(
                title: "Write the release notes",
                notes: RichText("Lead with the sync fix, then the new board."),
                stage: "To do",
            ),
        )
        b.items.insert(Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"))
        b.items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
    }
    if g.releases.is_empty() {
        let r = g.releases.insert(Release(title: "Release"))
        r.items.insert(Item(title: "Write the release notes", notes: RichText(""), stage: "To do"))
        r.items.insert(Item(title: "Translate the onboarding", notes: RichText(""), stage: "To do"))
        r.items.insert(Item(title: "Screenshots for the docs", notes: RichText(""), stage: "Doing"))
        r.items.insert(Item(title: "Bump the version", notes: RichText(""), stage: "Done"))
    }
    if g.data.is_empty() {
        let d = g.data.insert(Data(title: "Data"))
        d.rows.insert(Row(title: "Readme.md", state: "Edited", size: 1204))
        d.rows.insert(Row(title: "release.toml", state: "New", size: 312))
        d.rows.insert(Row(title: "notes.txt", state: "Locked", size: 48))
        let project = d.files.insert(Node(title: "Project", folder: true))
        let src = d.files.insert(Node(title: "src", parent: project, folder: true, size: 0))
        d.files.insert(Node(title: "main.rs", parent: src, folder: false, size: 880))
        d.files.insert(Node(title: "board.rs", parent: src, folder: false, size: 2416))
        let docs = d.files.insert(Node(title: "docs", parent: project, folder: true, size: 0))
        d.files.insert(Node(title: "release-notes.md", parent: docs, folder: false, size: 1204))
        d.messages.insert(
            Message(
                title: "a",
                from: "Ada",
                body: "Are we ready to ship the release tomorrow?",
                at: 2026-06-29T16:05Z,
                mine: false,
            ),
        )
        d.messages.insert(
            Message(
                title: "b",
                from: "Sam",
                body: "The build is green. One screenshot is left for the docs.",
                at: 2026-06-29T16:07Z,
                mine: true,
            ),
        )
        d.messages.insert(
            Message(
                title: "c",
                from: "Sam",
                body: "I'll finish it first thing.",
                at: 2026-06-29T16:07Z,
                mine: true,
            ),
        )
        d.messages.insert(
            Message(
                title: "d",
                from: "Ada",
                body: "Screenshots are in. Then let us go.",
                at: 2026-06-30T08:44Z,
                mine: false,
            ),
        )
        d.messages.insert(
            Message(
                title: "e",
                from: "Sam",
                body: "Shipping now.",
                at: 2026-06-30T08:46Z,
                mine: true,
            ),
        )
        d.cards.insert(Card(title: "Design", count: 12))
        d.cards.insert(Card(title: "Engineering", count: 34))
        d.cards.insert(Card(title: "Support", count: 7))
        d.changes.insert(
            Change(title: "a", who: "Ada", summary: "Rewrote the summary", at: 2026-06-30T08:42Z),
        )
        d.changes.insert(
            Change(title: "b", who: "Sam", summary: "Moved to Done", at: 2026-06-30T09:10Z),
        )
    }
    if g.media.is_empty() {
        let m = g.media.insert(
            Media(
                title: "Media",
                photo: photo(),
                at: 12.0,
                code: "tilo://invite/gallery",
                source: "text(\"Hills at dawn\")",
            ),
        )
        m.photos.insert(Photo(title: "Dawn", place: "Black Forest", picture: m.photo))
        m.photos.insert(Photo(title: "Blue hour", place: "Lake Constance"))
        m.photos.insert(Photo(title: "Dunes", place: "Erg Chebbi"))
        m.photos.insert(Photo(title: "Pines in fog", place: "Harz", tall: true))
    }
    if g.places.is_empty() { g.places.insert(Places(title: "Places")) }
    if g.chrome.is_empty() {
        let s = g.chrome.insert(ShellParts(title: "Shell", owner: invoker))
        s.apps.insert(Pinned(title: "Notes"))
        s.apps.insert(Pinned(title: "Tasks"))
        s.apps.insert(Pinned(title: "Mail"))
    }
    if g.customs.is_empty() {
        g.customs.insert(Custom(title: "Custom"))
    }
}
