use super::*;
use tilo_actions::{ActionInvoker, PathStep};

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
    assert_eq!(english.sentence(&created()), "New snippet in Snippets");

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
    assert_eq!(Reader::new("en-US").sentence(&bare), "Empty trash");
    assert_eq!(
        Reader::new("en-US").sentence(&ActionContext::default()),
        "Changed the document"
    );
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
            actor: "ada",
            kind: ActorKind::Agent,
            for_person: Some("ada"),
            for_reader: false,
            is_reader: false,
            task: Some(&task),
        }),
        "ada via task ‘Prepare Q3 financials’"
    );
    assert_eq!(
        Reader::new("de-DE").actor(&Who {
            actor: "ada",
            kind: ActorKind::Agent,
            for_person: Some("ada"),
            for_reader: false,
            is_reader: false,
            task: Some(&task),
        }),
        "ada über Aufgabe „Prepare Q3 financials“"
    );
    // A task nobody named leaves the actor as it is recorded.
    assert_eq!(
        reader.actor(&Who {
            actor: "ada",
            kind: ActorKind::Person,
            for_person: None,
            for_reader: false,
            is_reader: false,
            task: Some(&Task {
                id: task.id,
                label: " ",
            }),
        }),
        "ada"
    );
}

#[test]
fn actors_are_named_with_the_person_they_act_for() {
    let reader = Reader::new("en-US");
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("person:ada");
    assert_eq!(agent.kind, ActorKind::Agent);
    assert_eq!(
        reader.actor(&Who {
            actor: "Claude",
            kind: agent.kind,
            for_person: Some("ada"),
            for_reader: false,
            is_reader: false,
            task: None
        }),
        "Claude · Software, for ada"
    );
    assert_eq!(
        reader.actor(&Who {
            actor: "Claude",
            kind: ActorKind::Agent,
            for_person: None,
            for_reader: false,
            is_reader: false,
            task: None
        }),
        "Claude · Software"
    );
    assert_eq!(
        reader.actor(&Who {
            actor: "ada",
            kind: ActorKind::Person,
            for_person: None,
            for_reader: false,
            is_reader: true,
            task: None
        }),
        "You"
    );
    assert_eq!(
        Reader::new("de-DE").actor(&Who {
            actor: "Morgens",
            kind: ActorKind::Shortcut,
            for_person: Some("ada"),
            for_reader: false,
            is_reader: false,
            task: None
        }),
        "Morgens · Kurzbefehl, für ada"
    );
    assert_eq!(
        reader.actor(&Who {
            actor: "Discord Bridge",
            kind: ActorKind::Bot,
            for_person: Some("Ada"),
            for_reader: false,
            is_reader: false,
            task: None
        }),
        "Discord Bridge · Bot, for Ada"
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
            .with_app("so.tilo.lang.notes", "Notes")
            .with_described(
                Described::new(Did::verb(
                    ActionKey::new(
                        "so.tilo.lang.notes",
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
