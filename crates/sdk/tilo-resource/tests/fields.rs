//! Field options and the small conveniences around edits: atoms written
//! whole, skipped fields read as their type's default, a typed word as one
//! undo step, nullary destructive verbs asking first, and a replica's author
//! reaching its verbs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact values written and read back"
)]

use tilo_resource::{ActionCx, Id, Invoke, Model, Replica, resource};

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
}

impl Default for Board {
    fn default() -> Self {
        Self {
            layer: String::new(),
            title: String::new(),
            author: String::new(),
            admin: true,
            cards: 0,
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
