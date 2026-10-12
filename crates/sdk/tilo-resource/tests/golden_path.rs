#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use tilo_resource::{ActionCx, Collection, Id, Invoke, Model, Replica, Resource, resource};

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
    pub fn rename(&mut self, #[param("New title", example = "A shared board")] title: String) {
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
    #[action(destructive, field = "cards")]
    pub fn clear(&mut self) {
        self.cards = Collection::new();
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Channel {
    pub topic: String,
    pub read: i64,
}

#[resource(kind = "test.channel", label = "Channel")]
impl Channel {
    /// Set the topic.
    #[action]
    pub fn set_topic(&mut self, topic: String) {
        self.topic = topic;
    }

    /// Mark the channel read.
    #[action(housekeeping)]
    pub fn mark_read(&mut self, count: i64) {
        self.read = count;
    }
}

#[test]
fn a_housekeeping_action_says_so_in_its_change() {
    let housekeeping = |replica: &mut Replica<Channel>| {
        replica.versions().last().is_some_and(|version| {
            version
                .context
                .as_ref()
                .is_some_and(tilo_actions::ActionContext::is_housekeeping)
        })
    };
    let mut channel = Replica::<Channel>::new();
    channel.apply(Channel::MARK_READ.with(3)).unwrap();
    assert!(housekeeping(&mut channel));
    channel.apply(Channel::SET_TOPIC.with("Launch")).unwrap();
    assert!(!housekeeping(&mut channel));
    channel
        .apply(Invoke::group([Channel::MARK_READ.with(4)]))
        .unwrap();
    assert!(
        housekeeping(&mut channel),
        "a group of housekeeping is housekeeping"
    );
    channel
        .apply(Invoke::group([
            Channel::MARK_READ.with(5),
            Channel::SET_TOPIC.with("Ship"),
        ]))
        .unwrap();
    assert!(
        !housekeeping(&mut channel),
        "one content change makes it content"
    );
    assert_eq!(channel.state().unwrap().read, 5);
}

fn card_ids(replica: &Replica<Board>) -> Vec<Id> {
    replica.state().unwrap().cards.ids().cloned().collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn declared_actions_are_typed_and_apply_through_the_one_door() {
    // phase: methods_become_declared_typed_actions
    {
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
        assert_eq!(clear.field, Some("cards"));

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
    // phase: actions_apply_through_the_same_door_as_every_surface
    {
        let mut board = Replica::<Board>::new();
        board.apply(Board::RENAME.with("Launch")).unwrap();
        board.set_now_ms(42_000);
        board.apply(Board::ADD_CARD.with("Write docs")).unwrap();
        board.apply(Board::ADD_CARD.with("Ship")).unwrap();
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
            matches!(refused, Err(tilo_actions::DoorError::Failed(reason)) if reason == "no such card")
        );
        assert_eq!(board.heads(), heads);
    }
    // phase: a_group_is_one_change_that_replays_exactly
    {
        let mut first = Replica::<Board>::new();
        let mut second = first.fork();
        let before = first.versions().len();
        first
            .apply(Invoke::group([
                Board::ADD_CARD.with("milk"),
                Board::ADD_CARD.with("milk"),
                Board::RENAME.with("Shopping"),
            ]))
            .unwrap();
        let state = first.state().unwrap();
        assert_eq!(state.cards.len(), 2, "each member mints its own id");
        assert_eq!(state.title, "Shopping");
        assert_eq!(first.versions().len(), before + 1, "one change");
        for envelope in first.log().to_vec() {
            second.replay(&envelope).unwrap();
        }
        assert_eq!(first.heads(), second.heads());
        assert_eq!(second.state().unwrap(), state);

        // A member that refuses refuses the whole group.
        let missing = Id::from_raw("missing");
        assert!(
            first
                .apply(Invoke::group([
                    Board::ADD_CARD.with("eggs"),
                    Board::ADVANCE.with(missing),
                ]))
                .is_err()
        );
        assert_eq!(first.state().unwrap(), state);
    }
    // phase: actions_on_a_collection_item_are_offered_on_the_item
    {
        let mut board = Replica::<Board>::new();
        board.apply(Board::ADD_CARD.with("Write docs")).unwrap();
        board.apply(Board::ADD_CARD.with("Ship")).unwrap();
        let state = board.state().unwrap();
        let published = tilo_resource::publish::<Board>("board", "Board", &state);

        // One child resource per card, labelled by the card's text.
        let labels: Vec<_> = published
            .children
            .iter()
            .map(|child| child.label.as_str())
            .collect();
        assert_eq!(labels, ["Write docs", "Ship"]);
        let first = state.cards.ids().next().unwrap();
        let card = published.children.first().unwrap();
        assert_eq!(
            card.id.as_str(),
            tilo_resource::items::item_resource_id::<Board>("cards", first)
        );
        assert_eq!(
            tilo_resource::items::item_of_resource::<Board>(card.id.as_str()),
            Some(("cards", first.clone()))
        );

        // Its actions are the card actions, with the card already bound: only
        // the remaining parameters are asked for; and the card's own fields.
        let actions: Vec<_> = card.actions.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(
            actions,
            ["advance", "move-card", "delete-card", "field-edit"]
        );
        let advance = card.actions.first().unwrap();
        assert!(advance.params().is_empty());
        let move_card = card.actions.get(1).unwrap();
        let params: Vec<_> = move_card
            .params()
            .iter()
            .map(|param| param.name.as_ref())
            .collect();
        assert_eq!(params, ["index"]);
        let delete = card.actions.get(2).unwrap();
        assert_eq!(
            delete.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm,
            "a destructive item action still asks"
        );

        // The record keeps the id-taking action for scripts and agents, but a
        // person picks it on the card, so it is no palette row of its own.
        let on_record = published
            .actions
            .iter()
            .find(|spec| spec.id == "advance")
            .unwrap();
        assert!(on_record.is_programmatic());
        assert!(
            !published
                .actions
                .iter()
                .find(|spec| spec.id == "rename")
                .unwrap()
                .is_programmatic()
        );
    }
}

#[test]
fn documents_replay_converge_version_index_and_migrate() {
    // phase: replaying_the_log_reproduces_the_document_exactly
    {
        let mut origin = Replica::<Board>::new();
        let mut peer = origin.fork();
        origin.apply(Board::ADD_CARD.with("a")).unwrap();
        origin.apply(Board::ADD_CARD.with("b")).unwrap();
        let second = card_ids(&origin).remove(1);
        origin.apply(Board::MOVE_CARD.with((second, 0))).unwrap();
        for envelope in origin.log().to_vec() {
            peer.replay(&envelope).unwrap();
        }
        assert_eq!(peer.heads(), origin.heads());
        assert_eq!(peer.state().unwrap(), origin.state().unwrap());
    }
    // phase: collaborators_edit_concurrently_and_converge
    {
        let mut alice = Replica::<Board>::new().as_author("alice");
        alice.apply(Board::ADD_CARD.with("shared")).unwrap();
        let mut bob = alice.fork().as_author("bob");
        let shared = card_ids(&alice).remove(0);

        alice.apply(Board::RENAME.with("Alice's board")).unwrap();
        alice.apply(Board::ADVANCE.with(shared.clone())).unwrap();
        bob.apply(Board::ADD_CARD.with("from bob")).unwrap();
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
    // phase: history_is_attributed_and_any_version_reads_back
    {
        let mut board = Replica::<Board>::new().as_author("alice");
        board.apply(Board::RENAME.with("v1")).unwrap();
        let v1 = board.heads();
        board.apply(Board::RENAME.with("v2")).unwrap();
        let versions = board.versions();
        let last = versions.last().unwrap();
        assert_eq!(last.actor, hex::encode(b"alice"));
        let entry = last.context.as_ref().expect("an entry");
        let Some(tilo_actions::Did::Verb(verb)) = &entry.what else {
            panic!("the rename is recorded as its verb: {entry:?}");
        };
        assert_eq!(verb.action.action, "rename");
        assert_eq!(verb.args["title"].as_str(), Some("v2"));
        assert_eq!(
            entry.resource.as_ref().map(|r| r.label.as_str()),
            Some("Board")
        );
        assert_eq!(board.state_at(&v1).unwrap().title, "v1");
        assert_eq!(board.state().unwrap().title, "v2");
    }
    // phase: search_indexes_text_fields_by_default
    {
        let mut board = Replica::<Board>::new();
        board.apply(Board::RENAME.with("Roadmap")).unwrap();
        board
            .apply(Board::ADD_CARD.with("Quarterly review"))
            .unwrap();
        let entries = Board::search(&board.state().unwrap());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].text.contains("Quarterly review"));
    }
    // phase: a_record_reads_documents_written_by_an_older_schema
    {
        use automerge::transaction::Transactable;
        let mut doc = tilo_resource::Doc::new();
        doc.put(automerge::ROOT, "title", "legacy scalar title")
            .unwrap();
        let board = <Board as Resource>::read(&doc).unwrap();
        assert_eq!(board.title, "legacy scalar title");
        assert!(board.cards.is_empty());
    }
}

#[test]
fn apply_and_group_name_the_declared_actions_instead_of_repeating_them() {
    use serde_json::json;

    let published = tilo_resource::publish::<Board>("board", "Board", &Board::default());
    let schema = |id: &str| {
        published
            .actions
            .iter()
            .find(|spec| spec.id == id)
            .unwrap()
            .params()
            .first()
            .unwrap()
            .schema
            .clone()
            .unwrap()
    };
    let declared = json!([
        "rename",
        "add-card",
        "advance",
        "move-card",
        "delete-card",
        "clear"
    ]);
    let apply = schema(tilo_resource::APPLY);
    let operations = apply["properties"]["op"]["oneOf"].as_array().unwrap();
    assert_eq!(operations.len(), 2, "{apply}");
    assert!(
        operations
            .iter()
            .any(|operation| operation["properties"]["action"]["enum"] == declared),
        "{apply}"
    );
    assert!(
        operations.iter().any(
            |operation| operation["properties"]["action"]["const"] == tilo_resource::FIELD_EDIT
        ),
        "{apply}"
    );
    let group = schema(tilo_resource::GROUP);
    assert_eq!(
        group["items"]["oneOf"][0]["properties"]["action"]["enum"],
        declared
    );
    assert_eq!(
        group["items"]["oneOf"][1]["properties"]["action"]["const"],
        tilo_resource::FIELD_EDIT,
        "a gesture's field edits are members too"
    );
    assert_eq!(
        group["items"]["oneOf"][2]["properties"]["args"]["properties"][tilo_resource::APPLY_PARAM],
        tilo_resource::schema::undescribed(&apply)
    );

    let invoke = |value: serde_json::Value| {
        let mut args = serde_json::Map::new();
        args.insert(tilo_resource::APPLY_PARAM.into(), value);
        tilo_resource::build_action::<Board>(tilo_resource::APPLY, &args, 0)
    };
    assert!(invoke(json!({"op": {"action": "rename", "args": {"title": "Q3"}}})).is_ok());
    let refused = invoke(json!({"op": {"action": "rename", "args": {"title": 3}}})).unwrap_err();
    assert!(refused.starts_with("rename: "), "{refused}");
    let refused = invoke(json!({"op": {"action": "rename", "args": {"name": "Q3"}}})).unwrap_err();
    assert!(refused.contains("name"), "{refused}");

    let mut args = serde_json::Map::new();
    args.insert(
        tilo_resource::GROUP_PARAM.into(),
        json!([
            {"action": "rename", "args": {"title": "Q3"}},
            {"action": "move-card", "args": {"card": "c1"}},
        ]),
    );
    let refused = tilo_resource::group::build_group::<Board>(&args, 0).unwrap_err();
    assert!(refused.starts_with("member 1 (move-card): "), "{refused}");
}

#[test]
fn an_action_without_arguments_is_built_when_it_is_taken() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let takings = Arc::new(AtomicUsize::new(0));
    let clear = Board::declarations()
        .into_iter()
        .find(|decl| decl.id == "clear")
        .unwrap();
    let counted = Arc::clone(&takings);
    let decl = tilo_resource::ActionDecl::<Board>::new("clear", "Clear", Vec::new(), {
        move |args, now| {
            counted.fetch_add(1, Ordering::SeqCst);
            clear.build_action(args, now)
        }
    });
    let resource = tilo_actions::Resource::new("board", "test.board", "Board")
        .with_action(tilo_resource::declaration_spec(&decl));
    assert_eq!(
        takings.load(Ordering::SeqCst),
        0,
        "publishing builds nothing"
    );
    assert!(matches!(
        resource.actions[0].input,
        tilo_actions::ActionInput::Deferred { .. }
    ));
    let address = tilo_actions::ActionAddress::new("board", "clear");
    for taken in 1..=2 {
        let encoded =
            tilo_actions::door::encode(std::slice::from_ref(&resource), &address, None).unwrap();
        assert_eq!(encoded.type_id, Board::ACTION_TYPE);
        assert_eq!(
            takings.load(Ordering::SeqCst),
            taken,
            "each taking builds anew"
        );
    }

    let refusing =
        tilo_resource::ActionDecl::<Board>::new("refuse", "Refuse", Vec::new(), |_, _| {
            Err("nothing to clear".into())
        });
    let resource = tilo_actions::Resource::new("board", "test.board", "Board")
        .with_action(tilo_resource::declaration_spec(&refusing));
    let refused = tilo_actions::door::encode(
        std::slice::from_ref(&resource),
        &tilo_actions::ActionAddress::new("board", "refuse"),
        None,
    );
    assert!(
        matches!(&refused, Err(tilo_actions::DoorError::InvalidArguments(reason)) if reason == "nothing to clear"),
        "{refused:?}"
    );
}

#[test]
fn action_parameter_examples_are_guidance_not_a_value_restriction() {
    let rename = Board::declarations()
        .into_iter()
        .find(|decl| decl.id == "rename")
        .unwrap();
    assert_eq!(
        rename.params[0].schema.as_ref().unwrap()["examples"][0],
        "A shared board"
    );
    let supplied = serde_json::json!({"title": "Another valid title"});
    let bound =
        tilo_actions::ActionArgs::bind_json(&rename.params, supplied.as_object().unwrap()).unwrap();
    let action = rename.build_action(&bound, 0).unwrap();
    let mut replica = Replica::<Board>::new();
    replica.apply_action(&action).unwrap();
    assert_eq!(replica.state().unwrap().title, "Another valid title");
}

#[test]
fn schema_reuse_keeps_resource_types_and_live_children_distinct() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SCHEMA_BUILDS: AtomicUsize = AtomicUsize::new(0);
    struct SchemaProbe;
    impl Resource for SchemaProbe {
        const KIND: &'static str = "test.schema-probe";
        const LABEL: &'static str = "Schema probe";
        const ACTION_TYPE: &'static str = Board::ACTION_TYPE;
        type State = Board;
        type Action = <Board as Resource>::Action;
        fn read(doc: &tilo_resource::Doc) -> Result<Board, tilo_resource::ModelError> {
            Board::read(doc)
        }
        fn action_schema() -> Option<serde_json::Value> {
            SCHEMA_BUILDS.fetch_add(1, Ordering::SeqCst);
            let mut schema = Board::action_schema()?;
            schema["description"] = serde_json::json!("The schema probe's board action");
            Some(schema)
        }
        fn children(state: &Board) -> Vec<tilo_actions::Resource> {
            Board::children(state)
        }
    }
    let mut replica = Replica::<Board>::new();
    let empty = tilo_resource::publish::<SchemaProbe>("probe", "Empty", &replica.state().unwrap());
    assert!(empty.children.is_empty());
    replica.apply(Board::ADD_CARD.with("Fresh card")).unwrap();
    let populated =
        tilo_resource::publish::<SchemaProbe>("probe", "Populated", &replica.state().unwrap());
    assert_eq!(populated.label, "Populated");
    assert_eq!(
        populated.children.len(),
        1,
        "live rows are never cached with type schemas"
    );
    let schema = |resource: &tilo_actions::Resource| {
        resource
            .actions
            .iter()
            .find(|action| action.id == tilo_resource::APPLY)
            .unwrap()
            .params()
            .first()
            .unwrap()
            .schema
            .clone()
            .unwrap()
    };
    assert_eq!(schema(&empty), schema(&populated));
    assert!(
        std::sync::Arc::ptr_eq(&schema(&empty), &schema(&populated)),
        "type schemas share immutable storage while live children refresh"
    );
    let board = tilo_resource::publish::<Board>("board", "Board", &replica.state().unwrap());
    assert_ne!(
        schema(&board),
        schema(&populated),
        "resources sharing one action type keep their own declaration folding"
    );
    assert_eq!(
        SCHEMA_BUILDS.load(Ordering::SeqCst),
        1,
        "repeated publications and group reuse the compiled action schema"
    );
}

#[test]
fn the_head_is_the_published_record_without_building_a_child() {
    for count in [0_usize, 1, 3] {
        let board = Board {
            title: "Plan".to_owned(),
            cards: (0..count)
                .map(|index| {
                    let card = Card {
                        text: format!("Card {index}"),
                        stage: Stage::Todo,
                        created_ms: 0,
                    };
                    (Id::from_raw(format!("card-{index}")), card)
                })
                .collect(),
        };
        let mut published = tilo_resource::publish::<Board>("board", "Board", &board);
        assert_eq!(published.children.len(), count);
        published.children.clear();
        let head = tilo_resource::publish_head::<Board>("board", "Board", &board);
        assert!(head.children.is_empty());
        assert_eq!(
            tilo_actions::door::survey(&[head]),
            tilo_actions::door::survey(&[published]),
            "{count} cards: the head carries the published record's own actions"
        );
        // Counted, not built: the card actions are for scripts once a card
        // offers them.
        let offered = Board::children_offer_bound(&board);
        if count == 0 {
            assert!(offered.is_empty(), "{offered:?}");
        } else {
            assert_eq!(offered, ["advance", "move-card", "delete-card"]);
        }
    }
}
