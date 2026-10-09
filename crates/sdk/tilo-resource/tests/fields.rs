//! Field options and the small conveniences around edits: atoms written
//! whole, skipped fields read as their type's default, a typed word as one
//! undo step, nullary destructive verbs asking first, and a replica's author
//! reaching its verbs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact values written and read back"
)]

use tilo_resource::{ActionCx, Collection, Derived, Id, Invoke, Model, Replica, resource};

#[derive(Model, Debug, Clone, PartialEq)]
pub struct Board {
    /// The layer the board draws on, written whole.
    #[model(atom)]
    pub layer: String,
    pub title: String,
    pub author: String,
    #[model(skip)]
    pub admin: bool,
    pub cards: u32,
    pub notes: Collection<Note>,
    #[model(remove = "quiet")]
    pub drafts: Collection<String>,
}

/// How the board looks.
#[derive(
    Default,
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct BoardStyle {
    /// The board's title.
    pub title: String,
    /// How many cards it shows.
    pub cards: Option<u32>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    #[model(label)]
    pub title: String,
    pub body: Vec<String>,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            layer: String::new(),
            title: String::new(),
            author: String::new(),
            admin: true,
            cards: 0,
            notes: Collection::new(),
            drafts: Collection::new(),
        }
    }
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Remove every card.
    #[action(destructive)]
    pub fn clear(&mut self) {
        self.cards = 0;
    }

    /// Retitle the board and set its card count.
    #[action]
    pub fn restyle(&mut self, #[param(flatten)] style: BoardStyle) {
        self.title = style.title;
        if let Some(cards) = style.cards {
            self.cards = cards;
        }
    }

    /// Clear a note's lines, answering what it cleared.
    #[action(answers)]
    pub fn clear_note(&mut self, notes: &Id) {
        if let Some(note) = self.notes.get_mut(notes) {
            note.body.clear();
        }
    }

    /// Add a card, signed by whoever adds it.
    #[action]
    pub fn add_card(&mut self, cx: &mut ActionCx) {
        self.cards = self.cards.saturating_add(1);
        cx.author().unwrap_or_default().clone_into(&mut self.author);
    }
}

#[test]
fn an_atom_is_written_whole_and_the_last_writer_wins() {
    let mut a = Replica::<Board>::new();
    a.apply(Board::paths().layer.set("layer-1".into())).unwrap();
    let mut b = a.fork();
    a.apply(Board::paths().layer.set("layer-2".into())).unwrap();
    b.apply(Board::paths().layer.set("layer-3".into())).unwrap();
    a.sync_with(&mut b).unwrap();
    let merged = a.state().unwrap();
    assert_eq!(merged, b.state().unwrap());
    assert!(
        ["layer-2", "layer-3"].contains(&merged.layer.as_str()),
        "one of the two, not a mix of them: {}",
        merged.layer
    );
    assert!(
        a.apply(Board::paths().layer.splice(0, 0, "x")).is_err(),
        "an atom is not edited by character"
    );
}

#[test]
fn a_skipped_field_reads_as_the_containing_types_default() {
    let replica = Replica::<Board>::new();
    assert!(replica.state().unwrap().admin);
}

#[test]
fn a_space_ends_the_typed_word_it_follows() {
    let title = Board::paths().title;
    let typed = ["h", "e", "y", " ", "y", "o", "u"];
    let edits: Vec<_> = typed
        .iter()
        .enumerate()
        .map(|(at, text)| title.splice(at, 0, *text))
        .collect();
    let breaks: Vec<usize> = edits
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| !pair[0].coalesces(&pair[1]))
        .map(|(at, _)| at + 1)
        .collect();
    assert_eq!(breaks, [4], "`hey ` then `you`: undo leaves `hey `");
}

#[test]
fn a_nullary_destructive_verb_asks_first() {
    let mut board = Replica::<Board>::new();
    board.apply(Board::ADD_CARD.with(())).unwrap();
    assert!(matches!(
        board.apply(Board::CLEAR.with(())),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    board.apply_confirmed(Board::CLEAR.with(())).unwrap();
    assert_eq!(board.state().unwrap().cards, 0);
}

#[test]
fn a_replicas_author_is_the_invoker_its_verbs_see() {
    let mut board = Replica::<Board>::new().as_author("ada");
    board.apply(Board::ADD_CARD.with(())).unwrap();
    assert_eq!(board.state().unwrap().author, "ada");
    // An edit converts into an invocation where one is expected.
    let invoke: Invoke<Board> = Board::paths().title.set("Sprint".into()).into();
    board.apply(invoke).unwrap();
    assert_eq!(board.state().unwrap().title, "Sprint");
    assert_eq!(Id::default().as_str(), "");
}

#[test]
fn removing_an_item_by_field_edit_asks_unless_the_collection_is_quiet() {
    let mut board = Replica::<Board>::new();
    let paths = Board::paths();
    let note = Id::from_raw("n");
    board
        .apply(paths.notes.insert_as(&note, 0, &Note::default()))
        .unwrap();
    board
        .apply(paths.drafts.insert_as(&note, 0, &"draft".to_owned()))
        .unwrap();
    assert!(matches!(
        board.apply(paths.notes.remove(&note)),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    board.apply_confirmed(paths.notes.remove(&note)).unwrap();
    board.apply(paths.drafts.remove(&note)).unwrap();
    let state = board.state().unwrap();
    assert!(state.notes.is_empty() && state.drafts.is_empty());
}

#[test]
fn a_path_names_the_object_its_value_is_stored_in() {
    let mut board = Replica::<Board>::new();
    let paths = Board::paths();
    let note = Id::from_raw("n");
    board
        .apply(paths.notes.insert_as(&note, 0, &Note::default()))
        .unwrap();
    let body = paths.notes.item(&note).body.object(board.doc());
    assert!(
        body.is_some(),
        "the body's text object, under the entry's value"
    );
    assert!(
        paths
            .notes
            .item(&Id::from_raw("gone"))
            .body
            .object(board.doc())
            .is_none()
    );
    assert!(
        paths.cards.object(board.doc()).is_none(),
        "a scalar has no object"
    );
}

#[test]
fn a_derived_place_offers_a_bound_edit() {
    let place = Derived::<Board>::new("Sprint board").with_edit(
        "name-sprint",
        "Name it Sprint",
        Board::paths().title.set("Sprint".into()),
    );
    let published = place.publish();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == "name-sprint")
        .expect("the edit is offered on the place");
    assert!(spec.params().is_empty(), "its value is bound");
}

#[test]
fn a_flattened_struct_is_the_verbs_parameters() {
    let decl = <Board as tilo_resource::Resource>::declarations()
        .into_iter()
        .find(|decl| decl.id == "restyle")
        .unwrap();
    let params: Vec<(String, bool, tilo_actions::ParamType)> = decl
        .params
        .iter()
        .map(|param| (param.name.to_string(), param.required, param.ty))
        .collect();
    assert_eq!(
        params,
        [
            ("title".to_owned(), true, tilo_actions::ParamType::Text),
            ("cards".to_owned(), false, tilo_actions::ParamType::Integer),
        ]
    );
    let mut board = Replica::<Board>::new();
    board
        .apply(Board::RESTYLE.with(BoardStyle {
            title: "Sprint".into(),
            cards: Some(3),
        }))
        .unwrap();
    let state = board.state().unwrap();
    assert_eq!((state.title.as_str(), state.cards), ("Sprint", 3));
}

#[test]
fn a_derived_place_offers_an_edit_that_asks_for_its_value() {
    let place = Derived::<Board>::new("Sprint board").with_edit_asking(
        "rename",
        "Rename the board",
        "title",
        "The new name",
        |title: String| Board::paths().title.replace(title),
    );
    let published = place.publish();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == "rename")
        .expect("the edit is offered on the place");
    let params: Vec<(String, tilo_actions::ParamType)> = spec
        .params()
        .iter()
        .map(|param| (param.name.to_string(), param.ty))
        .collect();
    assert_eq!(
        params,
        [("title".to_owned(), tilo_actions::ParamType::Text)]
    );

    let encoded = spec
        .build_json(
            &serde_json::json!({ "title": "Sprint 12" })
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let action: <Board as tilo_resource::Resource>::Action =
        serde_json::from_slice(&encoded.payload).unwrap();
    let mut board = Replica::<Board>::new();
    board.apply_action(&action).unwrap();
    assert_eq!(board.state().unwrap().title, "Sprint 12");
}

/// What an agent sees of the field edit at `path` (`notes.*.body`): the
/// line describing it, and the example edit at it.
fn described(schema: &serde_json::Value, path: &str) -> (String, serde_json::Value) {
    let line = schema["description"]
        .as_str()
        .unwrap()
        .lines()
        .find(|line| line.starts_with(&format!("- `{path}`")))
        .expect("the descriptor describes the path")
        .to_owned();
    let dotted = |example: &serde_json::Value| {
        example["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["field"].as_str().unwrap_or("*").to_owned())
            .collect::<Vec<_>>()
            .join(".")
    };
    let example = schema["examples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|example| dotted(example) == path)
        .expect("an example edit at the path")
        .clone();
    (line, example)
}

#[test]
fn an_agent_builds_a_field_edit_from_the_published_descriptor_alone() {
    let mut board = Replica::<Board>::new();
    board
        .apply(
            Board::paths()
                .notes
                .insert_as(&Id::from_raw("n1"), 0, &Note::default()),
        )
        .unwrap();
    let published = board.published().unwrap();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .unwrap();
    let schema = spec.params()[0].schema.as_deref().unwrap().clone();

    // Every editable path is described: title, ops, value, flags.
    let (title, _) = described(&schema, "title");
    assert!(title.contains("ops: set, splice, replace"), "{title}");
    assert!(title.contains("\"string\""), "{title}");
    let (notes, _) = described(&schema, "notes");
    assert!(notes.contains("asks for confirmation"), "{notes}");
    let (drafts, _) = described(&schema, "drafts");
    assert!(drafts.contains("asks nothing"), "{drafts}");

    // Built from the descriptor: the example's path with the item's id in
    // place of its placeholder, an op it lists, a value of its type.
    let (_, example) = described(&schema, "notes.*.body");
    let mut path = example["path"].clone();
    for step in path.as_array_mut().unwrap() {
        if step.get("item").is_some() {
            *step = serde_json::json!({ "item": "n1" });
        }
    }
    let edit = serde_json::json!({ "path": path, "op": "set", "value": ["line"] });
    tilo_actions::validate_json_schema(&edit, &schema).expect("the edit fits the schema");
    let edit: tilo_resource::Edit<Board> = serde_json::from_value(edit).unwrap();
    board.apply(edit).unwrap();
    assert_eq!(
        board
            .state()
            .unwrap()
            .notes
            .get(&Id::from_raw("n1"))
            .unwrap()
            .body,
        ["line"]
    );
}

#[test]
fn an_item_resource_offers_the_field_edits_of_its_item() {
    let mut board = Replica::<Board>::new();
    board
        .apply(Board::paths().notes.insert_as(
            &Id::from_raw("n1"),
            0,
            &Note {
                title: "Plan".into(),
                ..Note::default()
            },
        ))
        .unwrap();
    let published = board.published().unwrap();
    let item = published
        .children
        .iter()
        .find(|child| child.id.as_str().ends_with("/notes/n1"))
        .expect("the note is an item resource");
    let spec = item
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .expect("its fields are edited from it");
    let schema = spec.params()[0].schema.as_deref().unwrap().clone();
    let (_, example) = described(&schema, "body");
    let edit =
        serde_json::json!({ "path": example["path"], "op": "set", "value": ["from the item"] });
    tilo_actions::validate_json_schema(&edit, &schema).expect("the edit fits the item's schema");
}

#[test]
fn an_answering_verb_published_on_its_item_still_answers() {
    let mut board = Replica::<Board>::new();
    board
        .apply(Board::paths().notes.insert_as(
            &Id::from_raw("n1"),
            0,
            &Note {
                title: "Plan".into(),
                ..Note::default()
            },
        ))
        .unwrap();
    let published = board.published().unwrap();
    let on_root = published
        .actions
        .iter()
        .find(|spec| spec.id == "clear-note")
        .unwrap();
    assert!(matches!(
        on_root.contract.result,
        tilo_actions::ResultSpec::Json { .. }
    ));
    let item = published
        .children
        .iter()
        .find(|child| child.id.as_str().ends_with("/notes/n1"))
        .unwrap();
    let on_item = item
        .actions
        .iter()
        .find(|spec| spec.id == "clear-note")
        .expect("offered on its item");
    assert_eq!(
        on_item.contract.result, on_root.contract.result,
        "the item's copy declares the same answer"
    );
}
