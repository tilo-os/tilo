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
