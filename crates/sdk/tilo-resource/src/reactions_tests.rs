use crate::{ActionCx, Collection, Id, Invoke, Model, Reactions, Replica, resource};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub cards: Collection<Card>,
    pub reactions: Reactions,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    #[model(label, readonly)]
    pub title: String,
    #[model(readonly)]
    pub reactions: Reactions,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Add a card.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.cards.push(
            cx.id(),
            Card {
                title,
                ..Card::default()
            },
        );
    }
}

fn board(author: &str) -> Replica<Board> {
    let mut replica = Replica::<Board>::new().as_author(author);
    replica.set_now_ms(1_000);
    replica
}

fn card(replica: &Replica<Board>) -> Id {
    replica.state().unwrap().cards.ids().next().unwrap().clone()
}

fn edit(replica: &mut Replica<Board>, edit: &crate::Edit<Board>) -> Result<(), String> {
    replica
        .apply(Invoke::edit(edit).unwrap())
        .map(drop)
        .map_err(|error| error.to_string())
}

fn counts(reactions: &Reactions, me: &str) -> Vec<(String, usize, bool)> {
    reactions
        .groups()
        .into_iter()
        .map(|group| {
            let (mine, count) = (group.includes(me), group.count());
            (group.emoji, count, mine)
        })
        .collect()
}

#[test]
fn a_reaction_is_its_invokers_own_on_items_and_records() {
    let mut ada = board("u_ada");
    ada.apply(Board::ADD.with("Ship it")).unwrap();
    let id = card(&ada);
    let mut bo = ada.fork().as_author("u_bo");

    // On an item that is otherwise read-only, and on the record itself.
    let on_card = Board::paths().cards.item(&id).reactions;
    edit(&mut ada, &on_card.react("🎉")).unwrap();
    edit(&mut bo, &on_card.react("🎉")).unwrap();
    bo.set_now_ms(2_000);
    edit(&mut bo, &on_card.react(":party_parrot:")).unwrap();
    edit(&mut bo, &Board::paths().reactions.react("👍")).unwrap();
    ada.sync_with(&mut bo).unwrap();

    let state = ada.state().unwrap();
    let reactions = &state.cards.get(&id).unwrap().reactions;
    assert_eq!(
        counts(reactions, "u_ada"),
        [
            ("🎉".to_owned(), 2, true),
            (":party_parrot:".to_owned(), 1, false),
        ],
        "concurrent first reactions with one emoji both count, the first emoji first"
    );
    assert!(reactions.reacted("🎉", "u_bo"));
    assert_eq!(
        counts(&state.reactions, "u_bo"),
        [("👍".to_owned(), 1, true)]
    );

    // Taking back is per person: Bo's leaves Ada's.
    edit(&mut bo, &on_card.unreact("🎉")).unwrap();
    edit(&mut bo, &on_card.unreact("🎉")).unwrap();
    ada.sync_with(&mut bo).unwrap();
    let state = ada.state().unwrap();
    assert_eq!(
        counts(&state.cards.get(&id).unwrap().reactions, "u_ada"),
        [
            ("🎉".to_owned(), 1, true),
            (":party_parrot:".to_owned(), 1, false),
        ]
    );
}

#[test]
fn reactions_refuse_what_is_no_emoji_and_generic_edits() {
    let mut ada = board("u_ada");
    let reactions = Board::paths().reactions;
    for not_emoji in ["", "  ", "lol", "👍 👍", ":a:", ":no spaces:", "x👍"] {
        assert!(
            edit(&mut ada, &reactions.react(not_emoji)).is_err(),
            "{not_emoji:?} is no emoji"
        );
    }
    assert!(edit(&mut ada, &reactions.react(" 👍🏽 ")).is_ok(), "trimmed");
    assert!(ada.state().unwrap().reactions.reacted("👍🏽", "u_ada"));
    // Nobody adds a reaction in someone else's name through a generic edit.
    let forged = crate::Edit::<Board>::new(
        vec![crate::Segment::Field("reactions".into())],
        crate::EditOp::Set {
            value: serde_json::json!([{"emoji": "👍", "by": "u_bo", "at": 0}]),
        },
    );
    assert!(edit(&mut ada, &forged).is_err());
    // A reaction is someone's.
    let mut nobody = Replica::<Board>::new();
    assert!(edit(&mut nobody, &reactions.react("👍")).is_err());
}

#[test]
fn reactions_hold_a_bounded_number_of_emoji() {
    let mut ada = board("u_ada");
    let reactions = Board::paths().reactions;
    let emoji: Vec<String> = (0..super::MAX_EMOJI)
        .map(|n| {
            char::from_u32(0x1F600 + u32::try_from(n).unwrap())
                .unwrap()
                .to_string()
        })
        .collect();
    for one in &emoji {
        edit(&mut ada, &reactions.react(one.as_str())).unwrap();
    }
    assert!(
        edit(&mut ada, &reactions.react("🚀")).is_err(),
        "one emoji too many"
    );
    let mut bo = ada.fork().as_author("u_bo");
    assert!(
        edit(&mut bo, &reactions.react(emoji[0].as_str())).is_ok(),
        "joining an emoji already there is fine"
    );
}

#[test]
fn react_and_unreact_toggle_and_read_back_as_json() {
    let mut ada = board("u_ada");
    let reactions = Board::paths().reactions;
    let toggle = |replica: &Replica<Board>| {
        let op = replica.state().unwrap().reactions.toggle("👍", "u_ada");
        crate::Edit::<Board>::new(vec![crate::Segment::Field("reactions".into())], op)
    };
    let first = toggle(&ada);
    edit(&mut ada, &first).unwrap();
    assert!(ada.state().unwrap().reactions.reacted("👍", "u_ada"));
    let json = ada.state().unwrap().reactions.to_json();
    assert_eq!(json[0]["emoji"], "👍");
    assert_eq!(json[0]["by"], "u_ada");
    assert_eq!(
        Reactions::from_json(&json).unwrap(),
        ada.state().unwrap().reactions
    );
    let second = toggle(&ada);
    edit(&mut ada, &second).unwrap();
    assert!(ada.state().unwrap().reactions.is_empty());
    let _ = reactions;
}

#[test]
fn custom_emoji_names_follow_discords_rule() {
    assert_eq!(
        super::custom_emoji_name(":party_parrot:"),
        Some("party_parrot")
    );
    assert_eq!(super::custom_emoji_name(":Tilo2:"), Some("Tilo2"));
    assert_eq!(super::custom_emoji_name(":x:"), None);
    assert_eq!(super::custom_emoji_name("party_parrot"), None);
    assert_eq!(super::custom_emoji_name(":has-dash:"), None);
    assert!(super::valid_custom_emoji_name(&"a".repeat(32)));
    assert!(!super::valid_custom_emoji_name(&"a".repeat(33)));
}
