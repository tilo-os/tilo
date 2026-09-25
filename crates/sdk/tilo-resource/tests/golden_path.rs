#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use tilo_resource::{ActionCx, Collection, Id, Model, Replica, Resource, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub title: String,
    pub cards: Collection<Card>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    pub text: String,
    pub stage: Stage,
    pub created_ms: i64,
}

#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Todo,
    Doing,
    Done,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Rename the board.
    #[action]
    pub fn rename(&mut self, #[param("New title")] title: String) {
        self.title = title;
    }

    /// Add a card.
    #[action]
    pub fn add_card(&mut self, cx: &mut ActionCx, text: String) {
        let created_ms = cx.now_ms();
        self.cards.push(
            cx.id(),
            Card {
                text,
                stage: Stage::Todo,
                created_ms,
            },
        );
    }

    /// Advance a card to the next stage.
    #[action]
    pub fn advance(&mut self, card: &Id) -> Result<(), &'static str> {
        let card = self.cards.get_mut(card).ok_or("no such card")?;
        card.stage = match card.stage {
            Stage::Todo => Stage::Doing,
            Stage::Doing | Stage::Done => Stage::Done,
        };
        Ok(())
    }

    /// Move a card to a position.
    #[action]
    pub fn move_card(&mut self, card: &Id, index: usize) {
        self.cards.move_to(card, index);
    }

    /// Delete a card.
    #[action(destructive)]
    pub fn delete_card(&mut self, card: &Id) {
        self.cards.remove(card);
    }

    /// Clear every card.
    #[action(destructive, permission = "admin")]
    pub fn clear(&mut self) {
        self.cards = Collection::new();
    }
}

fn card_ids(replica: &Replica<Board>) -> Vec<Id> {
    replica.state().unwrap().cards.ids().cloned().collect()
}

#[test]
fn methods_become_declared_typed_actions() {
    let decls = Board::declarations();
    let ids: Vec<_> = decls.iter().map(|decl| decl.id).collect();
    assert_eq!(
        ids,
        [
            "rename",
            "add-card",
            "advance",
            "move-card",
            "delete-card",
            "clear"
        ]
    );
    let rename = decls.first().unwrap();
    assert_eq!(rename.label, "Rename the board");
    assert_eq!(rename.params.first().unwrap().name, "title");
    let clear = decls.last().unwrap();
    assert!(clear.destructive);
    assert_eq!(clear.permission, tilo_actions::PermissionRequirement::Admin);

    // The published tree is what the palette, agents and scripts see.
    let published = tilo_resource::publish::<Board>("board", "Board", &Board::default());
    let published_ids: Vec<_> = published
        .actions
        .iter()
        .map(|spec| spec.id.as_str())
        .collect();
    assert!(published_ids.contains(&"add-card"));
    assert!(published_ids.contains(&tilo_resource::APPLY));
}

#[test]
fn actions_apply_through_the_same_door_as_every_surface() {
    let mut board = Replica::<Board>::new();
    board.apply(Board::RENAME.with("Launch".into())).unwrap();
    board.set_now_ms(42_000);
    board
        .apply(Board::ADD_CARD.with("Write docs".into()))
        .unwrap();
    board.apply(Board::ADD_CARD.with("Ship".into())).unwrap();
    let state = board.state().unwrap();
    assert_eq!(state.title, "Launch");
    let texts: Vec<_> = state
        .cards
        .values()
        .map(|card| card.text.as_str())
        .collect();
    assert_eq!(texts, ["Write docs", "Ship"]);
    assert!(state.cards.values().all(|card| card.created_ms == 42_000));

    let first = card_ids(&board).remove(0);
    board.apply(Board::ADVANCE.with(first.clone())).unwrap();
    assert_eq!(board.state().unwrap().cards[&first].stage, Stage::Doing);

    // A refusal is the app answering, and changes nothing.
    let heads = board.heads();
    let refused = board.apply(Board::ADVANCE.with(Id::from_raw("missing")));
    assert!(
        matches!(refused, Err(tilo_actions::ActionError::Refused(reason)) if reason == "no such card")
    );
    assert_eq!(board.heads(), heads);
}

#[test]
fn replaying_the_log_reproduces_the_document_exactly() {
    let mut origin = Replica::<Board>::new();
    let mut peer = origin.fork();
    origin.apply(Board::ADD_CARD.with("a".into())).unwrap();
    origin.apply(Board::ADD_CARD.with("b".into())).unwrap();
    let second = card_ids(&origin).remove(1);
    origin.apply(Board::MOVE_CARD.with((second, 0))).unwrap();
    for envelope in origin.log().to_vec() {
        peer.replay(&envelope).unwrap();
    }
    assert_eq!(peer.heads(), origin.heads());
    assert_eq!(peer.state().unwrap(), origin.state().unwrap());
}

#[test]
fn collaborators_edit_concurrently_and_converge() {
    let mut alice = Replica::<Board>::new().as_author("alice");
    alice.apply(Board::ADD_CARD.with("shared".into())).unwrap();
    let mut bob = alice.fork().as_author("bob");
    let shared = card_ids(&alice).remove(0);

    alice
        .apply(Board::RENAME.with("Alice's board".into()))
        .unwrap();
    alice.apply(Board::ADVANCE.with(shared.clone())).unwrap();
    bob.apply(Board::ADD_CARD.with("from bob".into())).unwrap();
    bob.apply(Board::MOVE_CARD.with((shared.clone(), 1)))
        .unwrap();

    alice.sync_with(&mut bob).unwrap();
    let a = alice.state().unwrap();
    assert_eq!(a, bob.state().unwrap());
    assert_eq!(a.title, "Alice's board");
    assert_eq!(a.cards.len(), 2);
    // Alice's field edit and Bob's move of the same card both survive.
    assert_eq!(a.cards[&shared].stage, Stage::Doing);
    assert_eq!(a.cards.position(&shared), Some(1));
}

#[test]
fn history_is_attributed_and_any_version_reads_back() {
    let mut board = Replica::<Board>::new().as_author("alice");
    board.apply(Board::RENAME.with("v1".into())).unwrap();
    let v1 = board.heads();
    board.apply(Board::RENAME.with("v2".into())).unwrap();
    let versions = board.versions();
    let last = versions.last().unwrap();
    assert_eq!(last.actor, hex(b"alice"));
    assert_eq!(last.summary().as_deref(), Some("Rename the board · Board"));
    assert_eq!(board.state_at(&v1).unwrap().title, "v1");
    assert_eq!(board.state().unwrap().title, "v2");
}

#[test]
fn search_indexes_text_fields_by_default() {
    let mut board = Replica::<Board>::new();
    board.apply(Board::RENAME.with("Roadmap".into())).unwrap();
    board
        .apply(Board::ADD_CARD.with("Quarterly review".into()))
        .unwrap();
    let entries = Board::search(&board.state().unwrap());
    assert_eq!(entries.len(), 1);
    assert!(entries[0].text.contains("Quarterly review"));
}

#[test]
fn a_record_reads_documents_written_by_an_older_schema() {
    use automerge::transaction::Transactable;
    let mut doc = tilo_resource::Doc::new();
    doc.put(automerge::ROOT, "title", "legacy scalar title")
        .unwrap();
    let board = <Board as Resource>::read(&doc).unwrap();
    assert_eq!(board.title, "legacy scalar title");
    assert!(board.cards.is_empty());
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
