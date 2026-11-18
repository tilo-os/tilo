//! What reaches a view of the shapes an app publishes beyond plain values, seen in a view over a
//! workspace, and that none of it reaches a viewer who may not read the app.

use serde_json::{Value as Json, json};
use tilo_actions::{Actor, MemberId};
use ui::written_view::ViewState as _;

use super::{TestWorkspace, ada, shown_reading, texts};

const SHEETS: &str = "so.tilo.sheets";
const TERMINAL: &str = "so.tilo.terminal";

/// A workbook of Sheets with the remarks `remarks`, each its words and the name of who made it,
/// as a read answers what Sheets stores: a collection is its items by id, each under its place,
/// and who made a remark is a case that carries a value, its name under `$variant` beside the
/// value under the number the case gives it.
fn workbook(remarks: &[(&str, &str)]) -> Json {
    let made: Vec<(&str, Json)> = remarks
        .iter()
        .map(|(words, by)| (*words, json!({ "$variant": "named", "0": by })))
        .collect();
    workbook_by(&made)
}

/// [`workbook`], each remark with who made it as Sheets stores that.
fn workbook_by(remarks: &[(&str, Json)]) -> Json {
    let mut stored = serde_json::Map::new();
    for (at, (words, by)) in remarks.iter().enumerate() {
        stored.insert(
            format!("c{at}"),
            json!({ "k": format!("a{at}"), "v": {
                "anchor": {
                    "sheet": "s1",
                    "area": { "top": "1", "left": "A", "bottom": "1", "right": "A" },
                },
                "author": by,
                "date": "2026-11-18",
                "text": words,
                "body": [],
                "resolved": false,
                "replies": {},
            } }),
        );
    }
    json!({ "comments": stored })
}

/// Each remark of the workbook [`BOOK`]: who made it, then its words. Sheets keeps a workbook
/// for each document, so the view names the one it means.
fn remarks() -> String {
    format!(
        "view remarks() {{\n    for c in @sheets.Workbook:{BOOK}.comments {{\n        match c.author {{\n            .member(_) => text(\"a member\"),\n            .named(name) => text(name),\n        }}\n        text(c.text)\n    }}\n}}"
    )
}

#[test]
fn a_case_that_carries_a_value_reaches_a_view_with_its_value() {
    let workspace = TestWorkspace::default();
    workspace.write_named(SHEETS, BOOK, workbook(&[("Check the sum", "Ada")]));
    let (mut surface, state) = shown_reading(&remarks(), ada(), &["sheets"], &workspace);
    surface.turn();
    assert_eq!(
        texts(&surface),
        ["Ada", "Check the sum"],
        "who made the remark, by the value its case carries, then its words: {:?}",
        surface.nodes("error")
    );

    // Someone adds a remark in Sheets: the view follows, case and value.
    workspace.write_named(
        SHEETS,
        BOOK,
        workbook(&[("Check the sum", "Ada"), ("Done", "Bo")]),
    );
    surface.turn();
    assert_eq!(texts(&surface), ["Ada", "Check the sum", "Bo", "Done"]);

    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn a_case_and_its_value_stay_unread_by_a_viewer_who_may_not_read_the_app() {
    let workspace = TestWorkspace::default();
    workspace.write_named(SHEETS, BOOK, workbook(&[("Check the sum", "Ada")]));
    workspace.refuse(SHEETS);
    let (mut surface, state) = shown_reading(&remarks(), ada(), &["sheets"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Sheets"]);
    assert!(
        texts(&surface).is_empty(),
        "neither the remark nor who made it is shown: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}

const MUSIC: &str = "so.tilo.music";

/// A library of Music with the albums `albums`, each its key, its title and its year, as a read
/// answers what Music stores: a map is the object of its entries.
fn library(albums: &[(&str, &str, i64)]) -> Json {
    let mut stored = serde_json::Map::new();
    for (key, title, year) in albums {
        stored.insert(
            (*key).to_owned(),
            json!({ "id": key, "title": title, "artist_id": "ar-1", "year": year }),
        );
    }
    json!({ "albums": stored })
}

/// Each album of the library by its key: its title, then its year.
const ALBUMS: &str = "view albums() {\n    for key in music.library.albums.keys() {\n        match music.library.albums.get(key) {\n            .none => text(\"gone\"),\n            .some(album) => {\n                text(album.title)\n                text(\"{album.year}\")\n            }\n        }\n    }\n}";

#[test]
fn a_map_reaches_a_view_by_its_keys_with_each_value() {
    let workspace = TestWorkspace::default();
    workspace.write(
        MUSIC,
        library(&[("al-2", "Low Tide", 1971), ("al-1", "High Water", 1976)]),
    );
    let (mut surface, state) = shown_reading(ALBUMS, ada(), &["music"], &workspace);
    assert_eq!(
        texts(&surface),
        ["High Water", "1976", "Low Tide", "1971"],
        "each album by its key, in the keys' order, its title and its year: {:?}",
        surface.nodes("error")
    );

    // An album is added and a year is put right: the view follows, key by key.
    workspace.write(
        MUSIC,
        library(&[
            ("al-2", "Low Tide", 1970),
            ("al-1", "High Water", 1976),
            ("al-0", "First Light", 1969),
        ]),
    );
    surface.turn();
    assert_eq!(
        texts(&surface),
        [
            "First Light",
            "1969",
            "High Water",
            "1976",
            "Low Tide",
            "1970"
        ]
    );

    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn a_map_stays_unread_by_a_viewer_who_may_not_read_the_app() {
    let workspace = TestWorkspace::default();
    workspace.write(MUSIC, library(&[("al-1", "High Water", 1976)]));
    workspace.refuse(MUSIC);
    let (mut surface, state) = shown_reading(ALBUMS, ada(), &["music"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Music"]);
    assert!(
        texts(&surface).is_empty(),
        "no key and no value of the map is shown: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}

/// Someone who neither wrote the view nor looks at it.
fn cy() -> MemberId {
    MemberId::person("cy")
}

/// One session of the terminal, by its id.
const SESSION: &str = "uuid:5d0c7a52-91e3-4a6f-8b1e-2f9c3d4e5a60";

/// Who holds the terminal's session [`SESSION`], by name. The terminal keeps a session for each
/// window, so the view names the one it means.
fn who_holds() -> String {
    format!(
        "view holder() {{\n    match @terminal.Session:{SESSION}.holder {{\n        .some(who) => text(who.name()),\n        .none => text(\"free\"),\n    }}\n}}"
    )
}

#[test]
fn an_actor_reaches_a_view_from_a_field_and_from_a_case_that_carries_one() {
    // A field that holds an actor, stored whole as one text of JSON: who holds the terminal's
    // session.
    let holder = serde_json::to_string(&Actor::Member(cy())).unwrap_or_default();
    let workspace = TestWorkspace::default();
    workspace.write_named(TERMINAL, SESSION, json!({ "holder": holder }));
    let (mut surface, state) = shown_reading(&who_holds(), ada(), &["terminal"], &workspace);
    surface.turn();
    assert_eq!(
        texts(&surface),
        ["cy"],
        "the session's holder, by name: {:?}",
        surface.nodes("error")
    );
    workspace.write_named(TERMINAL, SESSION, json!({ "holder": null }));
    surface.turn();
    assert_eq!(texts(&surface), ["free"]);
    assert!(state.refused().is_empty());

    // A case that carries an actor: a remark a member made, stored as the member alone, beside
    // one by someone only named.
    let member = serde_json::to_string(&cy()).unwrap_or_default();
    let workspace = TestWorkspace::default();
    workspace.write_named(
        SHEETS,
        BOOK,
        workbook_by(&[
            (
                "Check the sum",
                json!({ "$variant": "member", "0": member }),
            ),
            ("Done", json!({ "$variant": "named", "0": "Ada" })),
        ]),
    );
    let (mut surface, _) = shown_reading(&authors_of_the_book(), ada(), &["sheets"], &workspace);
    surface.turn();
    assert_eq!(
        texts(&surface),
        ["cy", "Check the sum", "Ada", "Done"],
        "who made each remark, a member by name: {:?}",
        surface.nodes("error")
    );
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn an_actor_stays_unread_by_a_viewer_who_may_not_read_the_app() {
    let holder = serde_json::to_string(&Actor::Member(cy())).unwrap_or_default();
    let workspace = TestWorkspace::default();
    workspace.write_named(TERMINAL, SESSION, json!({ "holder": holder }));
    workspace.refuse(TERMINAL);
    let (mut surface, state) = shown_reading(&who_holds(), ada(), &["terminal"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Terminal"]);
    assert!(
        !texts(&surface).iter().any(|text| text.contains("cy")),
        "nobody is said to hold the session: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}

const CHAT: &str = "so.tilo.chat";

/// What Chat keeps of how far each of `marks` (who, how far) has read its one conversation, as
/// a read answers what Chat stores: a map by person of a map by conversation.
fn chat(marks: &[(&str, i64)]) -> Json {
    let mut read = serde_json::Map::new();
    for (who, seen) in marks {
        read.insert(
            (*who).to_owned(),
            json!({ "c_chat": { "part": 0, "seen": seen } }),
        );
    }
    json!({ "read": read })
}

/// How far each person has read each conversation.
const MARKS: &str = "view marks() {\n    for who in chat.workspace.read.keys() {\n        match chat.workspace.read.get(who) {\n            .none => text(\"nothing\"),\n            .some(marks) => {\n                for conversation in marks.keys() {\n                    match marks.get(conversation) {\n                        .none => text(\"gone\"),\n                        .some(mark) => text(\"{who} in {conversation}: {mark.seen}\"),\n                    }\n                }\n            }\n        }\n    }\n}";

#[test]
fn a_map_inside_a_map_reaches_a_view_from_what_chat_keeps() {
    let workspace = TestWorkspace::default();
    workspace.write(CHAT, chat(&[("bo", 3)]));
    let (mut surface, state) = shown_reading(MARKS, ada(), &["chat"], &workspace);
    assert_eq!(
        texts(&surface),
        ["bo in c_chat: 3"],
        "how far each person has read each conversation: {:?}",
        surface.nodes("error")
    );

    // Someone reads on and someone else begins: the view follows both.
    workspace.write(CHAT, chat(&[("bo", 5), ("cy", 1)]));
    surface.turn();
    assert_eq!(texts(&surface), ["bo in c_chat: 5", "cy in c_chat: 1"]);
    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn what_chat_keeps_stays_unread_by_a_viewer_who_may_not_read_chat() {
    let workspace = TestWorkspace::default();
    workspace.write(CHAT, chat(&[("bo", 3)]));
    workspace.refuse(CHAT);
    let (mut surface, state) = shown_reading(MARKS, ada(), &["chat"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Chat"]);
    assert!(
        texts(&surface).is_empty(),
        "nothing anybody read is shown: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}

/// [`chat`] as Chat's store keeps it and a read answers it, each of `marks` who, the part they
/// read into and how far in it: a number that stands at 0 has no key, since a store keeps what
/// an absent key reads as by leaving it out. The first mark anyone makes is into part 0.
fn chat_as_stored(marks: &[(&str, i64, i64)]) -> Json {
    let mut read = serde_json::Map::new();
    for (who, part, seen) in marks {
        let mut mark = serde_json::Map::new();
        for (key, number) in [("part", part), ("seen", seen)] {
            if *number != 0 {
                mark.insert(key.to_owned(), json!(number));
            }
        }
        read.insert((*who).to_owned(), json!({ "c_chat": mark }));
    }
    json!({ "read": read })
}

/// How far each person has read each conversation, with the part they read into.
const PARTS: &str = "view parts() {\n    for who in chat.workspace.read.keys() {\n        match chat.workspace.read.get(who) {\n            .none => text(\"nothing\"),\n            .some(marks) => {\n                for conversation in marks.keys() {\n                    match marks.get(conversation) {\n                        .none => text(\"gone\"),\n                        .some(mark) => text(\"{who} in {conversation}: part {mark.part}, seen {mark.seen}\"),\n                    }\n                }\n            }\n        }\n    }\n}";

#[test]
fn what_a_store_leaves_out_reaches_a_view_as_the_value_it_stands_for() {
    let workspace = TestWorkspace::default();
    // A mark into part 0 is kept without its part.
    workspace.write(CHAT, chat_as_stored(&[("bo", 0, 3)]));
    let (mut surface, state) = shown_reading(PARTS, ada(), &["chat"], &workspace);
    assert_eq!(
        texts(&surface),
        ["bo in c_chat: part 0, seen 3"],
        "a mark kept without its part is a mark into part 0: {:?}",
        surface.nodes("error")
    );

    // A mark into a later part that saw nothing there yet, and one kept with no key at all.
    workspace.write(CHAT, chat_as_stored(&[("bo", 1, 0), ("cy", 0, 0)]));
    surface.turn();
    assert_eq!(
        texts(&surface),
        [
            "bo in c_chat: part 1, seen 0",
            "cy in c_chat: part 0, seen 0"
        ],
        "the view follows marks kept with fewer keys: {:?}",
        surface.nodes("error")
    );
    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn what_a_store_leaves_out_stays_unread_by_a_viewer_who_may_not_read_the_app() {
    let workspace = TestWorkspace::default();
    workspace.write(CHAT, chat_as_stored(&[("bo", 0, 0)]));
    workspace.refuse(CHAT);
    let (mut surface, state) = shown_reading(PARTS, ada(), &["chat"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Chat"]);
    assert!(
        texts(&surface).is_empty(),
        "no mark is shown, not even as the value it would start at: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}

/// One workbook of Sheets, by the id the tool that wrote into it answered with.
const BOOK: &str = "uuid:1a8f39eb-2404-48ac-9cd9-c7d8b1d1392f";

/// Each remark of the workbook [`BOOK`]: who made it, by name, then its words.
fn authors_of_the_book() -> String {
    format!(
        "view authors() {{\n    for c in @sheets.Workbook:{BOOK}.comments {{\n        match c.author {{\n            .member(who) => text(who.name()),\n            .named(name) => text(name),\n        }}\n        text(c.text)\n    }}\n}}"
    )
}

/// [`workbook_by`] as Sheets' store keeps it: a remark nobody settled has no `resolved`, since
/// a flag that is off has no key.
fn workbook_as_stored(remarks: &[(&str, Json)]) -> Json {
    let mut book = workbook_by(remarks);
    let stored = book.get_mut("comments").and_then(Json::as_object_mut);
    for entry in stored.into_iter().flat_map(|stored| stored.values_mut()) {
        if let Some(remark) = entry.get_mut("v").and_then(Json::as_object_mut) {
            remark.remove("resolved");
        }
    }
    book
}

#[test]
fn a_view_reads_the_remarks_of_one_workbook_named_by_its_id_and_follows_it() {
    // Sheets keeps a workbook as a document of its own and nothing for the workspace: a view
    // that names `sheets.workbook` alone reads nothing, so it names the workbook it means.
    let member = serde_json::to_string(&cy()).unwrap_or_default();
    let first = (
        "Check the sum",
        json!({ "$variant": "member", "0": member }),
    );
    let workspace = TestWorkspace::default();
    workspace.write_named(
        SHEETS,
        BOOK,
        workbook_as_stored(std::slice::from_ref(&first)),
    );
    let source = authors_of_the_book();
    let (mut surface, state) = shown_reading(&source, ada(), &["sheets"], &workspace);
    surface.turn();
    assert_eq!(
        texts(&surface),
        ["cy", "Check the sum"],
        "the remark of the workbook the view names, with who made it: {:?}",
        surface.nodes("error")
    );

    // Someone makes a second remark in that workbook: the view follows it.
    let second = ("Done", json!({ "$variant": "named", "0": "Ada" }));
    workspace.write_named(SHEETS, BOOK, workbook_as_stored(&[first, second]));
    surface.turn();
    assert_eq!(texts(&surface), ["cy", "Check the sum", "Ada", "Done"]);
    assert!(state.refused().is_empty());
    assert!(workspace.invoked().is_empty(), "reading sends nothing");
}

#[test]
fn a_workbook_named_by_its_id_stays_unread_by_a_viewer_who_may_not_read_sheets() {
    let member = serde_json::to_string(&cy()).unwrap_or_default();
    let remark = (
        "Check the sum",
        json!({ "$variant": "member", "0": member }),
    );
    let workspace = TestWorkspace::default();
    workspace.write_named(SHEETS, BOOK, workbook_as_stored(&[remark]));
    workspace.refuse(SHEETS);
    let source = authors_of_the_book();
    let (mut surface, state) = shown_reading(&source, ada(), &["sheets"], &workspace);
    surface.turn();
    assert_eq!(state.refused(), ["Sheets"]);
    assert!(
        texts(&surface).is_empty(),
        "no remark and nobody who made one is shown: {:?}",
        texts(&surface)
    );
    assert!(workspace.invoked().is_empty());
}
