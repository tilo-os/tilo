use super::*;
use tilo_actions::PathStep;

const SNIPPETS_EN: &str = r"
snippets-snippet-favorite = { $new ->
    [true] Starred { $item }
   *[false] Unstarred { $item }
}
snippets-snippet-folder = Moved { $item } to { $new }
snippets-new-snippet = Created { $title }
";

const SNIPPETS_DE: &str = r"
snippets-snippet-favorite = { $new ->
    [true] { $item } markiert
   *[false] Markierung von { $item } entfernt
}
snippets-snippet-folder = { $item } nach { $new } verschoben
snippets-new-snippet = { $title } erstellt
";

fn edit(field: &str, old: Option<Scalar>, new: Option<Scalar>) -> Did {
    Did::Edit(FieldEdit {
        ty: "snippet".into(),
        field: field.into(),
        path: vec![
            PathStep::Field("snippets".into()),
            PathStep::Item("s1".into()),
            PathStep::Field(field.into()),
        ],
        op: EditOp::Set,
        old,
        new,
        args: std::collections::BTreeMap::new(),
    })
}

fn entry(what: Did) -> ActionContext {
    ActionContext::for_resource("lib", "snippets.library", "Snippets")
        .with_app("org.tilo.Snippets", "Snippets")
        .with_described(Described::new(what).on("s1", "Deploy script"))
}

fn created() -> ActionContext {
    ActionContext::for_resource("lib", "snippets.library", "Snippets")
        .with_app("org.tilo.Snippets", "Snippets")
        .with_described(Described::new(Did::verb(
            ActionKey::new("org.tilo.Snippets", "snippets.library", "new-snippet"),
            [("title".to_string(), Scalar::from("Deploy script"))],
        )))
}

struct Published;

impl Labels for Published {
    fn field(&self, ty: &str, field: &str) -> Option<String> {
        (ty == "snippet" && field == "folder").then(|| "Folder".to_owned())
    }
}

#[test]
fn app_messages_render_in_the_readers_locale() {
    let en = Catalogue::new("en-US", &[SNIPPETS_EN]);
    let de = Catalogue::new("de-DE", &[SNIPPETS_DE]);
    let starred = entry(edit("favorite", Some(false.into()), Some(true.into())));
    let moved = entry(edit("folder", Some("Inbox".into()), Some("Ops".into())));

    let english = Reader::new("en-US").with_app(&en);
    assert_eq!(english.sentence(&starred), "Starred Deploy script");
    assert_eq!(english.sentence(&moved), "Moved Deploy script to Ops");
    assert_eq!(english.sentence(&created()), "Created Deploy script");

    let german = Reader::new("de_DE.UTF-8").with_app(&de);
    assert_eq!(german.sentence(&starred), "Deploy script markiert");
    assert_eq!(german.sentence(&moved), "Deploy script nach Ops verschoben");
    assert_eq!(german.sentence(&created()), "Deploy script erstellt");
}

#[test]
fn without_app_messages_the_framework_names_fields_by_their_labels() {
    let moved = entry(edit("folder", Some("Inbox".into()), Some("Ops".into())));
    let starred = entry(edit("favorite", Some(false.into()), Some(true.into())));
    let english = Reader::new("en-US").with_labels(&Published);
    assert_eq!(
        english.sentence(&moved),
        "Changed Folder of Deploy script to Ops"
    );
    assert_eq!(
        english.sentence(&starred),
        "Turned on Favorite for Deploy script"
    );
    assert_eq!(
        english.sentence(&created()),
        "Ran “New snippet” in Snippets"
    );

    let german = Reader::new("de-DE").with_labels(&Published);
    assert_eq!(
        german.sentence(&moved),
        "Folder von Deploy script auf Ops geändert"
    );
    assert_eq!(
        german.sentence(&starred),
        "Favorite für Deploy script eingeschaltet"
    );

    // A locale the framework has no messages for reads in the source locale.
    assert_eq!(
        Reader::new("fr-FR")
            .with_labels(&Published)
            .sentence(&moved),
        "Changed Folder of Deploy script to Ops"
    );
}

#[test]
fn sentences_never_show_raw_ids_or_paths() {
    let deep = entry(Did::Edit(FieldEdit {
        ty: "snippet".into(),
        field: "due_date".into(),
        path: vec![
            PathStep::Item("0f3c9a".into()),
            PathStep::Field("due_date".into()),
        ],
        op: EditOp::Replace,
        old: None,
        new: None,
        args: std::collections::BTreeMap::new(),
    }));
    let sentence = Reader::new("en-US").sentence(&deep);
    assert_eq!(sentence, "Edited Due date of Deploy script");
    assert!(!sentence.contains("0f3c9a") && !sentence.contains("due_date"));

    let mut bare = ActionContext::for_resource("lib", "snippets.library", "");
    bare.what = Some(Did::verb(
        ActionKey::new("org.tilo.Snippets", "snippets.library", "empty_trash"),
        [],
    ));
    assert_eq!(Reader::new("en-US").sentence(&bare), "Ran “Empty trash”");
    assert_eq!(
        Reader::new("en-US").sentence(&ActionContext::default()),
        "Changed the document"
    );
}

/// A verb with no app message reads as a past-tense sentence, in the reader's language: the
/// app's label quoted as the name it is, and what the change was about.
#[test]
fn a_verb_with_no_app_message_reads_as_a_sentence() {
    let on_item = entry(Did::verb(
        ActionKey::new("org.tilo.Snippets", "snippets.library", "empty_trash"),
        [],
    ));
    let within = ActionContext::for_resource("lib", "snippets.library", "Snippets").with_described(
        Described::new(Did::verb(
            ActionKey::new("org.tilo.Snippets", "snippets.library", "new-snippet"),
            [],
        )),
    );
    let unnamed = ActionContext::default().with_described(Described::new(Did::verb(
        ActionKey::new("org.tilo.Snippets", "snippets.library", "empty_trash"),
        [],
    )));
    let english = Reader::new("en-US");
    assert_eq!(
        english.sentence(&on_item),
        "Ran “Empty trash” on Deploy script"
    );
    assert_eq!(english.sentence(&within), "Ran “New snippet” in Snippets");
    assert_eq!(english.sentence(&unnamed), "Ran “Empty trash”");
    let german = Reader::new("de-DE");
    assert_eq!(
        german.sentence(&on_item),
        "„Empty trash“ auf Deploy script ausgeführt"
    );
    assert_eq!(
        german.sentence(&within),
        "„New snippet“ in Snippets ausgeführt"
    );
    assert_eq!(german.sentence(&unnamed), "„Empty trash“ ausgeführt");
}

#[test]
fn groups_and_history_steps_read_as_sentences() {
    let star = edit("favorite", Some(false.into()), Some(true.into()));
    let same = entry(Did::group([star.clone(), star.clone(), star.clone()]));
    let en = Catalogue::new("en-US", &[SNIPPETS_EN]);
    let reader = Reader::new("en-US").with_app(&en);
    assert_eq!(reader.sentence(&same), "Starred Deploy script (3×)");
    let mixed = entry(Did::group([
        star.clone(),
        edit("folder", None, Some("Ops".into())),
    ]));
    assert_eq!(reader.sentence(&mixed), "2 changes to Deploy script");

    let undone = ActionContext::default()
        .with_app("org.tilo.Snippets", "Snippets")
        .with_described(Described::new(Did::history(
            HistoryOp::Undo,
            1,
            Some(Described::new(star).on("s1", "Deploy script")),
        )));
    assert_eq!(reader.sentence(&undone), "Undid “Starred Deploy script”");
    assert_eq!(
        Reader::new("de-DE")
            .with_app(&Catalogue::new("de-DE", &[SNIPPETS_DE]))
            .sentence(&undone),
        "„Deploy script markiert“ rückgängig gemacht"
    );
    let many = ActionContext::default().with_described(Described::new(Did::history(
        HistoryOp::Undo,
        3,
        None,
    )));
    assert_eq!(reader.sentence(&many), "Undid 3 steps");
    assert_eq!(
        Reader::new("de-DE").sentence(&many),
        "3 Schritte rückgängig gemacht"
    );
    let checkpoint = ActionContext::default().with_described(Described::new(Did::history(
        HistoryOp::Checkpoint {
            name: "Before launch".into(),
        },
        0,
        None,
    )));
    assert_eq!(
        reader.sentence(&checkpoint),
        "Saved checkpoint “Before launch”"
    );
}

#[test]
fn a_task_names_the_person_who_acted_through_it() {
    let reader = Reader::new("en-US");
    let task = Task {
        id: "11111111-1111-4111-8111-111111111111",
        label: "Prepare Q3 financials",
    };
    // An agent working a task is named by the person under whose authority
    // the task ran, through the task.
    assert_eq!(
        reader.actor(&Who {
            actor: "Ada",
            is_reader: false,
            task: Some(&task),
        }),
        "Ada via task ‘Prepare Q3 financials’"
    );
    assert_eq!(
        Reader::new("de-DE").actor(&Who {
            actor: "Ada",
            is_reader: false,
            task: Some(&task),
        }),
        "Ada über Aufgabe „Prepare Q3 financials“"
    );
    // A task nobody named leaves the actor as it is recorded.
    assert_eq!(
        reader.actor(&Who {
            actor: "Ada",
            is_reader: false,
            task: Some(&Task {
                id: task.id,
                label: " ",
            }),
        }),
        "Ada"
    );
}

#[test]
fn an_actor_is_named_by_its_own_name_alone() {
    let reader = Reader::new("en-US");
    assert_eq!(
        reader.actor(&Who {
            actor: "Claude",
            is_reader: false,
            task: None
        }),
        "Claude"
    );
    assert_eq!(
        reader.actor(&Who {
            actor: "Ada",
            is_reader: true,
            task: None
        }),
        "You"
    );
}

#[test]
fn message_keys_follow_the_convention() {
    assert_eq!(app_prefix("org.tilo.WordProcessor"), "word-processor");
    assert_eq!(app_prefix("so.tilo.mail"), "mail");
    assert_eq!(
        message_key("org.tilo.Snippets", &edit("favorite", None, None)).as_deref(),
        Some("snippets-snippet-favorite")
    );
    assert_eq!(
        message_key("", created().what.as_ref().unwrap()).as_deref(),
        Some("snippets-new-snippet")
    );
}

#[test]
fn an_edit_the_fields_type_describes_passes_its_facts_by_name() {
    const WP: &str = r"
word-processor-document-body = { $op ->
    [mark] { $on ->
        [true] Made { $words } words { $mark } in { $item }
       *[false] Removed { $mark } from { $words } words in { $item }
    }
   *[other] Edited { $item }
}
";
    let mut args = std::collections::BTreeMap::new();
    args.insert("mark".to_owned(), Scalar::from("bold"));
    args.insert("on".to_owned(), Scalar::from(true));
    args.insert("words".to_owned(), Scalar::from(3));
    let bold = ActionContext::default()
        .with_app("org.tilo.WordProcessor", "Writer")
        .with_described(
            Described::new(Did::Edit(FieldEdit {
                ty: "document".into(),
                field: "body".into(),
                path: vec![PathStep::Field("body".into())],
                op: EditOp::Mark,
                old: None,
                new: None,
                args,
            }))
            .on("doc", "Report"),
        );
    let catalogue = Catalogue::new("en-US", &[WP]);
    assert_eq!(
        Reader::new("en-US").with_app(&catalogue).sentence(&bold),
        "Made 3 words bold in Report"
    );
}

#[test]
fn an_element_a_set_gained_or_lost_is_named() {
    let tag = |op, tag: &str| {
        entry(Did::Edit(FieldEdit {
            ty: "snippet".into(),
            field: "tags".into(),
            path: vec![
                PathStep::Field("snippets".into()),
                PathStep::Item("s1".into()),
                PathStep::Field("tags".into()),
            ],
            op,
            old: None,
            new: Some(tag.into()),
            args: std::collections::BTreeMap::new(),
        }))
    };
    let english = Reader::new("en-US");
    assert_eq!(
        english.sentence(&tag(EditOp::Insert, "ops")),
        "Added ops to Tags of Deploy script"
    );
    assert_eq!(
        english.sentence(&tag(EditOp::Remove, "ops")),
        "Removed ops from Tags of Deploy script"
    );
}

#[test]
fn a_resource_the_platform_made_reads_as_its_creation() {
    let made = |label: &str| {
        ActionContext::default()
            .with_app("so.tilo.notes", "Notes")
            .with_described(
                Described::new(Did::verb(
                    ActionKey::new(
                        "so.tilo.notes",
                        "tl.notes.Note",
                        tilo_actions::CREATE_ACTION,
                    ),
                    [],
                ))
                .on("root-1", label),
            )
    };
    let english = Reader::new("en-US");
    assert_eq!(english.sentence(&made("Plan")), "Created Plan");
    assert_eq!(english.sentence(&made("")), "Created");
    let german = Reader::new("de-DE");
    assert_eq!(german.sentence(&made("Plan")), "Plan erstellt");
    assert_eq!(german.sentence(&made("")), "Erstellt");
}

#[test]
fn a_step_the_app_has_no_message_for_says_so_and_reads_the_fallback() {
    let moved = entry(edit("folder", Some("Inbox".into()), Some("Ops".into())));
    let what = moved.what.as_ref().unwrap();
    // The app's catalogue has the field, so it renders the sentence itself.
    let snippets = Catalogue::new("en-US", &[SNIPPETS_EN]);
    assert_eq!(
        Reader::new("en-US").with_app(&snippets).did_by_app(
            what,
            "org.tilo.Snippets",
            Some("Deploy script")
        ),
        ("Moved Deploy script to Ops".to_owned(), true)
    );
    // A catalogue with no message for it falls back, and says so, so an
    // agent or a check can tell the framework's words from the app's.
    let other = Catalogue::new("en-US", &["snippets-snippet-favorite = Starred"]);
    assert_eq!(
        Reader::new("en-US").with_app(&other).did_by_app(
            what,
            "org.tilo.Snippets",
            Some("Deploy script")
        ),
        ("Changed Folder of Deploy script to Ops".to_owned(), false)
    );
    // Without an app catalogue at all nothing is the app's.
    assert!(
        !Reader::new("en-US")
            .did_by_app(what, "org.tilo.Snippets", None)
            .1
    );
}

#[test]
fn a_group_names_every_message_one_of_its_steps_asks_for() {
    let star = edit("favorite", Some(false.into()), Some(true.into()));
    let move_to = edit("folder", None, Some("Ops".into()));
    let app = "org.tilo.Snippets";
    assert_eq!(
        message_keys(app, &star),
        ["snippets-snippet-favorite".to_owned()]
    );
    // The same step three times renders through its one message.
    assert_eq!(
        message_keys(app, &Did::group([star.clone(), star.clone()])),
        ["snippets-snippet-favorite".to_owned()]
    );
    // A mixed group renders one generic sentence: the distinct steps it
    // names are what its catalogue must still carry for the group's own row.
    let mixed = Did::group([star, move_to]);
    let mut keys = message_keys(app, &mixed);
    keys.sort();
    assert_eq!(
        keys,
        [
            "snippets-snippet-favorite".to_owned(),
            "snippets-snippet-folder".to_owned()
        ]
    );
    // A history step is the framework's to word, never the app's.
    assert!(message_keys(app, &Did::history(HistoryOp::Undo, 1, None)).is_empty());
}

#[test]
fn a_framework_message_a_catalogue_lacks_never_renders_as_its_id() {
    // Every message the reader asks for by name is in the framework's own
    // catalogue: `tilo-history/locales/en-US.ftl`. Read through a catalogue
    // that holds none of them, an entry still reads as words.
    let bare = Catalogue::new("en-US", &["unrelated = nothing to do with history"]);
    let moved = entry(edit("folder", Some("Inbox".into()), Some("Ops".into())));
    let sentence = Reader::new("en-US").with_app(&bare).sentence(&moved);
    assert_eq!(sentence, "Changed Folder of Deploy script to Ops");
    assert!(!sentence.contains("history-"), "{sentence}");
}

/// A window speaks one language: the person's when it ships it, whatever else it ships.
#[test]
fn a_window_speaks_one_language() {
    assert_eq!(
        window_language(Some("de_AT.UTF-8"), &["en-US", "de-DE"]),
        "de-DE",
        "their language, in the region the app ships"
    );
    assert_eq!(
        window_language(Some("de-DE"), &["en-US"]),
        "en-US",
        "an app that ships English only speaks English"
    );
    assert_eq!(
        window_language(Some("fr-FR"), &["de-DE", "en-US", "fr-FR"]),
        "fr-FR",
        "the language they prefer, when it ships it"
    );
    assert_eq!(
        window_language(Some("fr-FR"), &["de-DE", "en-US"]),
        "en-US",
        "no language of theirs: the source language"
    );
    assert_eq!(
        window_language(Some("fr-FR"), &["ja-JP", "de-DE"]),
        "ja-JP",
        "no language of theirs and no source: the first it ships"
    );
    assert_eq!(
        window_language(None, &["de-DE", "en-US"]),
        "en-US",
        "no preference: the source language"
    );
    assert_eq!(
        window_language(Some("de-DE"), &[]),
        "de-DE",
        "nothing to choose among: the person's own"
    );
}

/// An item labelled only by its id (a record with no title yet) is said as unnamed: a row never
/// reads "Renamed 8a6a72fc-…".
#[test]
fn an_item_labelled_by_its_id_is_said_as_unnamed() {
    let named = |id: &str, label: &str| tilo_actions::ActionItem {
        id: id.into(),
        label: label.into(),
    };
    assert_eq!(item_label(&named("page:1", "Plan")), Some("Plan"));
    assert_eq!(item_label(&named("page:abc", "abc")), None);
    assert_eq!(
        item_label(&named("x", "8a6a72fc-2051-4bc1-9a3e-1f2d3c4b5a69")),
        None
    );
    assert_eq!(item_label(&named("x", "  ")), None);
    // A name that only shares letters with its id is a name: a channel's "chat" and a theme's
    // "Ember".
    assert_eq!(item_label(&named("c_chat", "chat")), Some("chat"));
    assert_eq!(item_label(&named("ember", "Ember")), Some("Ember"));
}

/// A step that carried an empty title reads as its action, never as an app's message with a
/// hole where the title goes.
#[test]
fn an_empty_value_leaves_the_message_that_needs_it_to_the_action() {
    let en = Catalogue::new("en-US", &[SNIPPETS_EN]);
    let untitled = ActionContext::for_resource("lib", "snippets.library", "Snippets")
        .with_app("org.tilo.Snippets", "Snippets")
        .with_described(Described::new(Did::verb(
            ActionKey::new("org.tilo.Snippets", "snippets.library", "new-snippet"),
            [("title".to_string(), Scalar::from(""))],
        )));
    let reader = Reader::new("en-US").with_app(&en);
    assert_eq!(reader.sentence(&created()), "Created Deploy script");
    assert_eq!(reader.sentence(&untitled), "Ran “New snippet” in Snippets");
}

/// A step names its item, and an id it carried, by the name the reader's app gives it now: a
/// note typed after it was made reads as what it says, and an id nothing names says nothing.
#[test]
fn a_step_names_its_item_as_the_app_calls_it_now() {
    struct Now;
    impl Labels for Now {
        fn item(&self, id: &str) -> Option<String> {
            (id == NOTE).then(|| "Garden plan".to_owned())
        }
    }
    const NOTE: &str = "9a8898a9-d93d-4cfd-8e57-3c79686c254b";
    const GONE: &str = "1b8898a9-d93d-4cfd-8e57-3c79686c254b";
    let notes = Catalogue::new(
        "en-US",
        &["notes-page-body = Edited “{ $item }”\nnotes-new-note = Created “{ $note }”"],
    );
    let app = |what: Described| {
        ActionContext::for_resource("pages", "notes.pages", "Notes")
            .with_app("so.tilo.notes", "Notes")
            .with_described(what)
    };
    let body = Did::Edit(FieldEdit {
        ty: "page".into(),
        field: "body".into(),
        path: vec![PathStep::Field("body".into())],
        op: EditOp::Splice,
        args: std::collections::BTreeMap::new(),
        old: None,
        new: None,
    });
    // An item with no name when the step was taken is recorded by its id.
    let typed = app(Described::new(body).on(NOTE, NOTE));
    let made = |note: &str| {
        app(Described::new(Did::verb(
            ActionKey::new("so.tilo.notes", "notes.pages", "new-note"),
            [
                ("note".to_owned(), Scalar::from(note)),
                ("title".to_owned(), Scalar::from("")),
            ],
        )))
    };
    let reader = Reader::new("en-US").with_app(&notes).with_labels(&Now);
    assert_eq!(reader.sentence(&typed), "Edited “Garden plan”");
    assert_eq!(reader.sentence(&made(NOTE)), "Created “Garden plan”");
    assert_eq!(reader.sentence(&made(GONE)), "Ran “New note” in Notes");
}

/// A change that made an item, though its caller named none (an agent asking for a note by its
/// title alone), says the item by `$now`: as the reader's app calls it now, so a note renamed
/// since reads under its name, while `$item` keeps the name the change recorded.
#[test]
fn a_change_names_the_item_it_made_as_it_is_called_now() {
    struct Now;
    impl Labels for Now {
        fn item(&self, id: &str) -> Option<String> {
            (id == NOTE).then(|| "Roadmap".to_owned())
        }
    }
    const NOTE: &str = "9a8898a9-d93d-4cfd-8e57-3c79686c254b";
    const GONE: &str = "1b8898a9-d93d-4cfd-8e57-3c79686c254b";
    let notes = Catalogue::new(
        "en-US",
        &["notes-new-note = Created “{ $now }”\nnotes-new-child = Made “{ $item }”"],
    );
    let made = |action: &str, note: &str| {
        ActionContext::for_resource("pages", "notes.pages", "Notes")
            .with_app("so.tilo.notes", "Notes")
            .with_described(
                Described::new(Did::verb(
                    ActionKey::new("so.tilo.notes", "notes.pages", action),
                    [("title".to_owned(), Scalar::from("Plan"))],
                ))
                .on(note, "Plan"),
            )
    };
    let reader = Reader::new("en-US").with_app(&notes).with_labels(&Now);
    assert_eq!(
        reader.sentence(&made("new-note", NOTE)),
        "Created “Roadmap”"
    );
    assert_eq!(reader.sentence(&made("new-child", NOTE)), "Made “Plan”");
    // An item gone since is said as the change recorded it.
    assert_eq!(reader.sentence(&made("new-note", GONE)), "Created “Plan”");
}

/// What an app publishes an action as, with the words it says the action did.
fn published(
    action: &str,
    label: &str,
    did: Option<&str>,
) -> tilo_actions::published::PublishedAction {
    let mut published = tilo_actions::published::PublishedAction::offered(
        &tilo_actions::published::Offer {
            resource: "notes.pages",
            resource_kind: "notes.pages",
            action,
            label,
            description: None,
            kind: tilo_actions::ActionKind::Undoable,
            operation: tilo_actions::OperationKind::Document,
            confirm: false,
            asks: Vec::new(),
            keywords: Vec::new(),
            icon: None,
        },
        "Notes",
    );
    published.did = did.map(str::to_owned);
    published
}

/// A step on a note, by a verb of Notes named `action`.
fn on_note(action: &str) -> ActionContext {
    ActionContext::for_resource("pages", "notes.pages", "Notes")
        .with_app("so.tilo.notes", "Notes")
        .with_described(
            Described::new(Did::verb(
                ActionKey::new("so.tilo.notes", "notes.note", action),
                [],
            ))
            .on("n1", "Spring plans"),
        )
}

/// A reader that is not the app (the session's History of a workspace) words a step by what
/// the app published: what the action did, starting the row and naming the item it did it to,
/// whichever spelling of the action's id the change and the surface use.
#[test]
fn a_verb_reads_in_the_words_its_app_says_it_did() {
    let mut labels = ActionLabels::default();
    labels.learn_published(
        "so.tilo.notes",
        &[
            published(
                "append-to-note",
                "Append to note",
                Some("added to the note"),
            ),
            published("pin", "Pin", None),
        ],
    );
    let english = Reader::new("en-US").with_labels(&labels);
    assert_eq!(
        english.sentence(&on_note("append_to_note")),
        "Added to the note “Spring plans”"
    );
    // An action that declares nothing it did is named by its label.
    assert_eq!(
        english.sentence(&on_note("pin")),
        "Ran “Pin” on Spring plans"
    );
    let german = Reader::new("de-DE").with_labels(&labels);
    assert_eq!(
        german.sentence(&on_note("append-to-note")),
        "Added to the note: „Spring plans“"
    );
    // Unnamed, and in the resource it ran in.
    let unnamed = ActionContext::default().with_described(Described::new(Did::verb(
        ActionKey::new("so.tilo.notes", "notes.pages", "append-to-note"),
        [],
    )));
    assert_eq!(english.sentence(&unnamed), "Added to the note");
    let within = ActionContext::for_resource("pages", "notes.pages", "Notes").with_described(
        Described::new(Did::verb(
            ActionKey::new("so.tilo.notes", "notes.pages", "append-to-note"),
            [],
        )),
    );
    assert_eq!(english.sentence(&within), "Added to the note in Notes");
}

/// The app's own message still wins over what it says the action did, and another app's action
/// of the same name never lends this one its words.
#[test]
fn an_app_message_wins_and_another_apps_words_are_not_borrowed() {
    let mut labels = ActionLabels::default();
    labels.learn_published(
        "so.tilo.chat",
        &[published("append-to-note", "Append", Some("appended"))],
    );
    let reader = Reader::new("en-US").with_labels(&labels);
    assert_eq!(
        reader.sentence(&on_note("append-to-note")),
        "Ran “Append to note” on Spring plans"
    );
    labels.learn_published(
        "so.tilo.notes",
        &[published(
            "append-to-note",
            "Append to note",
            Some("added to the note"),
        )],
    );
    let notes = Catalogue::new(
        "en-US",
        &["notes-append-to-note = Wrote more in “{ $item }”"],
    );
    let reader = Reader::new("en-US").with_app(&notes).with_labels(&labels);
    assert_eq!(
        reader.sentence(&on_note("append-to-note")),
        "Wrote more in “Spring plans”"
    );
}

#[test]
fn a_record_the_reader_holds_itself_reads_in_its_declared_words() {
    // The session holds the workspace's agents itself: no app publishes them, and the change
    // records whichever app id applied it.
    let mut labels = ActionLabels::default();
    labels.learn_declared(
        "tilo.workspace.agent",
        [("create_agent", "Create agent", Some("added the agent"))],
    );
    let reader = Reader::new("en-US").with_labels(&labels);
    for app in ["org.tilo.Tilo", "so.tilo.workspace", "so.tilo.chat"] {
        let made =
            ActionContext::for_resource("tilo.workspace.agent", "tilo.workspace.agent", "Agents")
                .with_described(
                    Described::new(Did::verb(
                        ActionKey::new(app, "tilo.workspace.agent", "create-agent"),
                        [],
                    ))
                    .on("agent-1", "Claude Code"),
                );
        assert_eq!(
            reader.sentence(&made),
            "Added the agent “Claude Code”",
            "{app}"
        );
    }
}
