//! Field options and the small conveniences around edits: atoms written
//! whole, skipped fields read as their type's default, a typed word as one
//! undo step, nullary destructive verbs asking first, and a replica's author
//! reaching its verbs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
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

/// The title as the origin resolves it: upper case.
#[allow(clippy::unnecessary_wraps, reason = "a prepare hook may refuse")]
fn shouted(params: &mut board::Shout) -> Result<(), String> {
    params.title = params.title.to_uppercase();
    Ok(())
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

    /// Retitle the board loudly: the title is resolved once, where invoked.
    #[action(prepare = shouted)]
    pub fn shout(&mut self, title: String) {
        self.title = title;
    }

    /// Retitle the board, with an optional suffix.
    #[action]
    pub fn retitle(&mut self, title: String, suffix: Option<String>) {
        self.title = format!("{title}{}", suffix.unwrap_or_default());
    }

    /// Clear a note's lines, answering what it cleared.
    #[action(answers)]
    pub fn clear_note(&mut self, notes: &Id) {
        if let Some(note) = self.notes.get_mut(notes) {
            note.body.clear();
        }
    }

    /// Note who a card was added for, and by what.
    #[action]
    pub fn add_card_for(&mut self, cx: &mut ActionCx) {
        self.cards = self.cards.saturating_add(1);
        self.author = format!(
            "{:?} for {}",
            cx.actor_kind().unwrap_or_default(),
            cx.for_person().unwrap_or_default()
        );
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

/// A text field bound to a record replaces the whole text on every keystroke.
/// Those replacements still undo a word at a time, like splices: a space
/// ends the word it follows, erasing runs on, and a paste is its own step.
#[test]
fn whole_text_replacements_undo_a_word_at_a_time() {
    let title = Board::paths().title;
    let texts = [
        "h",
        "he",
        "hey",
        "hey ",
        "hey y",
        "hey yo",
        "hey you",
        "hey yo",
        "hey y",
        "hey yesterday",
        "hey yesterday!",
    ];
    let edits: Vec<_> = texts.iter().map(|text| title.replace(*text)).collect();
    let breaks: Vec<usize> = edits
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| !pair[0].coalesces(&pair[1]))
        .map(|(at, _)| at + 1)
        .collect();
    assert_eq!(
        breaks,
        [4, 9],
        "`hey ` is one step, `you` erased back to `y` another, the pasted \
         `esterday` a third, typed on by `!`"
    );
}

/// A destructive verb of the record is undoable, so it applies at once:
/// nobody is asked first.
#[test]
fn a_nullary_destructive_verb_applies_at_once() {
    let mut board = Replica::<Board>::new();
    board.apply(Board::ADD_CARD.with(())).unwrap();
    board.apply(Board::CLEAR.with(())).unwrap();
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

/// A field edit changes only the record: removing an item, of any
/// collection, is undoable and asks nothing.
#[test]
fn removing_an_item_by_field_edit_asks_nothing() {
    let mut board = Replica::<Board>::new();
    let paths = Board::paths();
    let note = Id::from_raw("n");
    board
        .apply(paths.notes.insert_as(&note, 0, &Note::default()))
        .unwrap();
    board
        .apply(paths.drafts.insert_as(&note, 0, &"draft".to_owned()))
        .unwrap();
    board.apply(paths.notes.remove(&note)).unwrap();
    board.apply(paths.drafts.remove(&note)).unwrap();
    let state = board.state().unwrap();
    assert!(state.notes.is_empty() && state.drafts.is_empty());
}

/// A door without the app's code — the broker, a relay — decides from the
/// published field edit alone, and as the app does: the record's edits are
/// undoable, so none asks, whatever it removes.
#[test]
fn any_door_reads_a_field_edit_s_confirmation_from_the_published_contract() {
    use tilo_actions::ConfirmationPolicy;
    let board = Replica::<Board>::new();
    let published = board.published().unwrap();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .unwrap();
    let paths = Board::paths();
    let note = Id::from_raw("n");
    let door = |edit: &dyn erased_edit::Encode| {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "edit": edit.encode() })
        );
        spec.contract.confirmation_for_argument(Some(&argument))
    };
    assert_eq!(door(&paths.notes.remove(&note)), ConfirmationPolicy::None);
    assert_eq!(door(&paths.drafts.remove(&note)), ConfirmationPolicy::None);
    assert_eq!(
        door(&paths.title.set("Plan".to_owned())),
        ConfirmationPolicy::None
    );
    assert_eq!(
        spec.contract.confirmation_for_argument(Some("not an edit")),
        ConfirmationPolicy::None,
        "an argument nobody can read builds nothing; Undo covers whatever does"
    );
}

/// A lock whose delay can weaken security and whose shown name can expose
/// its owner.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Lock {
    #[model(trust = "weakening")]
    pub minutes: u32,
    #[model(trust = "sensitive")]
    pub shown_name: String,
    pub label: String,
}

#[resource(kind = "test.lock", label = "Lock")]
impl Lock {}

/// A field's trust decides its every edit, in the app and at any door that
/// reads only the published contract: a weakening one asks and is
/// privileged, a sensitive one asks, an ordinary one neither.
#[test]
fn a_guarded_field_asks_before_any_edit_and_a_weakening_one_is_privileged() {
    use tilo_actions::{ConfirmationPolicy, RiskLevel};
    let paths = Lock::paths();
    assert_eq!(paths.minutes.set(10).risk(), RiskLevel::Privileged);
    assert_eq!(
        paths.shown_name.set("Ada".to_owned()).risk(),
        RiskLevel::Destructive
    );
    assert_eq!(paths.label.set("Desk".to_owned()).risk(), RiskLevel::Low);

    let mut lock = Replica::<Lock>::new();
    assert!(matches!(
        lock.apply(paths.minutes.set(0)),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    assert!(matches!(
        lock.apply(paths.shown_name.set("Ada".to_owned())),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    lock.apply(paths.label.set("Desk".to_owned())).unwrap();
    lock.apply_confirmed(paths.minutes.set(0)).unwrap();
    assert_eq!(lock.state().unwrap().minutes, 0);

    let published = lock.published().unwrap();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .unwrap();
    let door = |edit: &dyn erased_edit::Encode| {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "edit": edit.encode() })
        );
        spec.contract.confirmation_for_argument(Some(&argument))
    };
    assert_eq!(door(&paths.minutes.set(5)), ConfirmationPolicy::Confirm);
    assert_eq!(
        door(&paths.shown_name.set(String::new())),
        ConfirmationPolicy::Confirm
    );
    assert_eq!(
        door(&paths.label.set(String::new())),
        ConfirmationPolicy::None
    );
    let need = |edit: &dyn erased_edit::Encode| {
        let serde_json::Value::Object(json) = serde_json::json!({ "edit": edit.encode() }) else {
            panic!("an object");
        };
        spec.need_for(Some(&json), None)
    };
    assert!(
        need(&paths.minutes.set(5)).privileged,
        "a weakening edit needs the person's own grant"
    );
    assert!(!need(&paths.shown_name.set(String::new())).privileged);
    assert!(!need(&paths.label.set(String::new())).privileged);
}

/// The field edit setting `value` at the fields `path`, as a caller sends
/// it.
fn set_at<R: tilo_resource::Resource>(path: &[&str], value: &str) -> Invoke<R> {
    let path = path
        .iter()
        .map(|key| tilo_resource::Segment::Field((*key).to_owned()))
        .collect();
    let op = tilo_resource::EditOp::Set {
        value: serde_json::json!(value),
    };
    Invoke::edit(&tilo_resource::Edit::new(path, op)).unwrap()
}

/// A record read that nests its fields under the record's name is written
/// back as `board.title`: a path starting at the record's own name (the
/// last part of its kind, or its label) edits the field after it, through
/// the published field edit and when built from JSON alike.
#[test]
fn a_field_edit_may_start_at_the_record_s_own_name() {
    let mut board = Replica::<Board>::new();
    board.apply(set_at(&["board", "title"], "Plan")).unwrap();
    board.apply(set_at(&["Board", "author"], "Ada")).unwrap();
    let state = board.state().unwrap();
    assert_eq!(
        (state.title.as_str(), state.author.as_str()),
        ("Plan", "Ada")
    );

    let built = tilo_resource::build_action::<Board>(
        tilo_resource::FIELD_EDIT,
        &serde_json::Map::from_iter([(
            "edit".to_owned(),
            serde_json::json!({ "path": [{ "field": "board" }, { "field": "title" }], "op": "set", "value": "Sprint" }),
        )]),
        0,
    );
    assert!(built.is_ok(), "{built:?}");
}

/// A field edit naming no field, nor the record, still says which fields
/// there are; one naming the record alone says to start at a field.
#[test]
fn a_field_edit_naming_neither_a_field_nor_the_record_lists_the_fields() {
    let mut board = Replica::<Board>::new();
    let unknown = format!(
        "{:?}",
        board.apply(set_at(&["deck", "title"], "Plan")).unwrap_err()
    );
    assert!(
        unknown.contains("no field `deck`; the fields here are layer, title, author"),
        "{unknown}"
    );
    let below = format!(
        "{:?}",
        board.apply(set_at(&["board", "deck"], "Plan")).unwrap_err()
    );
    assert!(
        below.contains("no field `deck`; the fields here are"),
        "the field after the record's name is the one refused: {below}"
    );
    let bare = format!("{:?}", board.apply(set_at(&["board"], "Plan")).unwrap_err());
    assert!(
        bare.contains("`board` is the record itself") && bare.contains("title"),
        "{bare}"
    );
    assert_eq!(board.state().unwrap().title, "");
}

/// A door decides an edit's confirmation from the path as sent: a guarded
/// field under the record's name is not edited, but sent again from the
/// root, where the door sees which field it is.
#[test]
fn a_guarded_field_under_the_record_s_name_is_sent_again_from_the_root() {
    let mut lock = Replica::<Lock>::new();
    for field in ["minutes", "shown_name"] {
        let refused = format!(
            "{:?}",
            lock.apply_confirmed(set_at(&["lock", field], "5"))
                .unwrap_err()
        );
        assert!(
            refused.contains(&format!("start the path at `{field}`")),
            "{refused}"
        );
    }
    lock.apply(set_at(&["lock", "label"], "Desk")).unwrap();
    let state = lock.state().unwrap();
    assert_eq!((state.minutes, state.label.as_str()), (0, "Desk"));
}

/// A screen whose lock delay weakens when longer (`0` is never) and whose
/// notifications on the lock screen weaken it when shown.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Screen {
    #[model(weaker = "higher", weakest = 0)]
    pub lock_minutes: u32,
    #[model(weaker = true)]
    pub notifications_when_locked: bool,
    #[model(weaker = ["strict", "normal", "off"])]
    pub guard: String,
}

#[resource(kind = "test.screen", label = "Screen")]
impl Screen {}

/// A field that weakens one way asks, and needs the person's own grant,
/// only for a change that way from what it holds now: in the app and at any
/// door that reads only the published contract.
#[test]
fn only_a_change_making_a_field_weaker_asks_and_is_privileged() {
    use tilo_actions::{ConfirmationPolicy, RiskLevel};
    let paths = Screen::paths();
    assert_eq!(
        paths.lock_minutes.set(1).risk(),
        RiskLevel::Privileged,
        "without the value now, any change counts as weakening"
    );

    let mut screen = Replica::<Screen>::new();
    screen
        .apply(paths.lock_minutes.set(10))
        .expect("from never, a delay is stricter");
    screen
        .apply(paths.lock_minutes.set(5))
        .expect("sooner is stricter");
    screen
        .apply(paths.lock_minutes.set(5))
        .expect("the same is no weaker");
    for weaker in [15, 0] {
        assert!(matches!(
            screen.apply(paths.lock_minutes.set(weaker)),
            Err(tilo_actions::DoorError::ConfirmationRequired)
        ));
    }
    assert!(matches!(
        screen.apply(paths.notifications_when_locked.set(true)),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    screen.apply(paths.guard.set("strict".to_owned())).unwrap();
    assert!(matches!(
        screen.apply(paths.guard.set("off".to_owned())),
        Err(tilo_actions::DoorError::ConfirmationRequired)
    ));
    screen
        .apply_confirmed(paths.notifications_when_locked.set(true))
        .unwrap();
    screen
        .apply(paths.notifications_when_locked.set(false))
        .expect("hiding them is stricter");
    assert_eq!(screen.state().unwrap().lock_minutes, 5);

    let published = screen.published().unwrap();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .unwrap();
    let door = |edit: &dyn erased_edit::Encode| {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "edit": edit.encode() })
        );
        spec.contract.confirmation_for_argument(Some(&argument))
    };
    assert_eq!(door(&paths.lock_minutes.set(1)), ConfirmationPolicy::None);
    assert_eq!(
        door(&paths.lock_minutes.set(30)),
        ConfirmationPolicy::Confirm
    );
    assert_eq!(
        door(&paths.lock_minutes.set(0)),
        ConfirmationPolicy::Confirm
    );
    assert_eq!(
        door(&paths.notifications_when_locked.set(true)),
        ConfirmationPolicy::Confirm
    );
    assert_eq!(
        door(&paths.guard.set("strict".to_owned())),
        ConfirmationPolicy::None
    );
    assert_eq!(
        door(&paths.guard.set("normal".to_owned())),
        ConfirmationPolicy::Confirm
    );

    // An agent acting for the person needs their own grant only to weaken.
    let need = |edit: &dyn erased_edit::Encode| {
        let serde_json::Value::Object(json) = serde_json::json!({ "edit": edit.encode() }) else {
            panic!("an object");
        };
        spec.need_for(Some(&json), None)
    };
    assert!(need(&paths.lock_minutes.set(30)).privileged);
    assert!(need(&paths.lock_minutes.set(0)).privileged);
    assert!(need(&paths.notifications_when_locked.set(true)).privileged);
    assert!(!need(&paths.lock_minutes.set(1)).privileged);
    assert!(!need(&paths.lock_minutes.set(5)).privileged);
    assert!(!need(&paths.notifications_when_locked.set(false)).privileged);
}

/// The JSON form of an edit, whatever record it is of.
mod erased_edit {
    pub trait Encode {
        fn encode(&self) -> serde_json::Value;
    }
    impl<R> Encode for tilo_resource::Edit<R> {
        fn encode(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap()
        }
    }
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
/// line describing it, and an edit at it built from the example of its
/// shape (one example per shape of path: a field, an item's field, …).
fn described(schema: &serde_json::Value, path: &str) -> (String, serde_json::Value) {
    let line = schema["description"]
        .as_str()
        .unwrap()
        .lines()
        .find(|line| line.starts_with(&format!("- `{path}`")))
        .expect("the descriptor describes the path")
        .to_owned();
    let segments: Vec<&str> = path.split('.').collect();
    let mut example = schema["examples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|example| {
            let steps = example["path"].as_array().unwrap();
            steps.len() == segments.len()
                && steps
                    .iter()
                    .zip(&segments)
                    .all(|(step, segment)| (*segment == "*") == step.get("item").is_some())
        })
        .expect("an example edit of the path's shape")
        .clone();
    for (step, segment) in example
        .get_mut("path")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap()
        .iter_mut()
        .zip(&segments)
    {
        if *segment != "*" {
            *step = serde_json::json!({ "field": segment });
        }
    }
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
    assert!(title.contains("`title`: set, splice, replace"), "{title}");
    assert!(title.contains("(string)"), "{title}");
    let (notes, _) = described(&schema, "notes");
    assert!(notes.contains("Undo takes it back"), "{notes}");
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

/// What to look at.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct LookAt {
    /// The region, as x, y, width, height.
    pub region: Vec<u32>,
    /// Whether to draw the grid.
    pub grid: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "test.board.view")]
pub enum BoardView {
    /// Look at part of the board.
    #[effect(view)]
    Look {
        #[param(flatten)]
        look: LookAt,
    },
}

#[test]
fn a_flattened_struct_is_the_effects_arguments() {
    use tilo_resource::Effect as _;
    let decl = BoardView::declarations().into_iter().next().unwrap();
    let params: Vec<(String, bool, tilo_actions::ParamType)> = decl
        .params
        .iter()
        .map(|param| (param.name.to_string(), param.required, param.ty))
        .collect();
    assert_eq!(
        params,
        [
            ("region".to_owned(), true, tilo_actions::ParamType::Array),
            ("grid".to_owned(), false, tilo_actions::ParamType::Boolean),
        ]
    );
    let look = BoardView::Look {
        look: LookAt {
            region: vec![0, 0, 10, 10],
            grid: Some(true),
        },
    };
    let arguments = look.arguments();
    assert_eq!(arguments["region"], serde_json::json!([0, 0, 10, 10]));
    assert_eq!(BoardView::decode("look", &arguments).unwrap(), look);
}

#[test]
fn an_optional_parameter_bound_to_nothing_is_left_out() {
    let decl = Board::RETITLE
        .bind(board::Retitle::paths().title, "Sprint".to_owned())
        .and(board::Retitle::paths().suffix, None)
        .declaration("retitle-sprint", "Call it Sprint")
        .unwrap();
    let action = decl
        .build_action(&tilo_actions::ActionArgs::default(), 0)
        .expect("it builds");
    let mut board = Replica::<Board>::new();
    board.apply_action(&action).unwrap();
    assert_eq!(board.state().unwrap().title, "Sprint");
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Pair::matched)]
pub struct Pair {
    pub left: u32,
    pub right: u32,
}

impl Pair {
    fn matched(&self) -> Result<(), tilo_resource::Refusal> {
        if self.left == self.right {
            Ok(())
        } else {
            Err(tilo_resource::Refusal::new("the two sides differ"))
        }
    }
}

#[resource(kind = "test.pair", label = "Pair")]
impl Pair {
    /// Move the left side.
    #[action]
    pub fn move_left(&mut self, to: u32) {
        self.left = to;
    }
}

#[test]
fn a_group_is_checked_once_at_its_end() {
    let mut pair = Replica::<Pair>::new();
    let paths = Pair::paths();
    assert!(
        pair.apply(paths.left.set(1)).is_err(),
        "alone it breaks the pair"
    );
    pair.apply(Invoke::group([
        paths.left.set(1).into(),
        paths.right.set(1).into(),
    ]))
    .expect("together they keep it");
    pair.apply(Invoke::group([
        Pair::MOVE_LEFT.with(2),
        paths.right.set(2).into(),
    ]))
    .expect("a verb and an edit together keep it too");
    assert!(
        pair.apply(Invoke::group([
            paths.left.set(3).into(),
            paths.right.set(4).into()
        ]))
        .is_err(),
        "a group that ends broken is refused"
    );
    let state = pair.state().unwrap();
    assert_eq!((state.left, state.right), (2, 2));
}

/// A control whose gesture is sometimes several actions is bound to a
/// group; when it is one, it records that action alone, so an edit in it
/// continues the undo step of the edit before as the edit itself would.
#[test]
fn a_group_of_one_is_that_action() {
    let mut pair = Replica::<Pair>::new();
    let paths = Pair::paths();
    let alone = pair
        .apply(Pair::MOVE_LEFT.with(0))
        .expect("a verb")
        .action
        .clone();
    let grouped = pair
        .apply(Invoke::group([Pair::MOVE_LEFT.with(0)]))
        .expect("a group of one verb")
        .action
        .clone();
    assert_eq!(grouped.type_id, alone.type_id);
    let pair_of = pair
        .apply(Invoke::group([
            paths.left.set(0).into(),
            paths.right.set(0).into(),
        ]))
        .expect("a group of two")
        .action
        .clone();
    assert_ne!(pair_of.type_id, alone.type_id, "two actions stay a group");
}

#[test]
fn a_place_offers_a_verb_with_nothing_bound() {
    let place = Derived::<Board>::new("Board").with(Board::RETITLE.unbound());
    let published = place.publish();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == "retitle")
        .expect("offered on the place");
    let params: Vec<String> = spec.params().iter().map(|p| p.name.to_string()).collect();
    assert_eq!(params, ["title", "suffix"]);
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Element {
    #[model(label)]
    pub name: String,
    pub x: f64,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Slide {
    #[model(label)]
    pub title: String,
    pub elements: Collection<Element>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Deck {
    pub slides: Collection<Slide>,
}

#[resource(kind = "test.deck", label = "Deck")]
impl Deck {
    /// Nudge an element along.
    #[action(item = slides.elements)]
    pub fn nudge(&mut self, slides: &Id, elements: &Id, by: f64) -> Result<(), &'static str> {
        let element = self
            .slides
            .get_mut(slides)
            .and_then(|slide| slide.elements.get_mut(elements))
            .ok_or("no such element")?;
        element.x += by;
        Ok(())
    }
}

#[test]
fn items_of_a_nested_collection_are_resources_of_their_own() {
    let mut deck = Replica::<Deck>::new();
    let mut slide = Slide {
        title: "Intro".into(),
        ..Slide::default()
    };
    slide.elements.push(
        Id::from_raw("e1"),
        Element {
            name: "Logo".into(),
            x: 1.0,
        },
    );
    deck.apply(
        Deck::paths()
            .slides
            .insert_as(&Id::from_raw("s1"), 0, &slide),
    )
    .unwrap();
    let published = deck.published().unwrap();
    let element = published
        .children
        .iter()
        .find(|child| child.id.as_str() == "test.deck/slides/s1/elements/e1")
        .expect("the element is a resource of its own");
    assert_eq!(element.label, "Logo");
    let edit = element
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .expect("its fields are edited from it");
    let encoded = edit
        .build_json(
            serde_json::json!({ "edit": { "path": [{"field": "x"}], "op": "set", "value": 4.5 } })
                .as_object()
                .unwrap(),
        )
        .unwrap();
    let action: <Deck as tilo_resource::Resource>::Action =
        serde_json::from_slice(&encoded.payload).unwrap();
    deck.apply_action(&action).unwrap();

    // The verbs on elements are offered on each, both ids bound.
    let published = deck.published().unwrap();
    let element = published
        .children
        .iter()
        .find(|child| child.id.as_str() == "test.deck/slides/s1/elements/e1")
        .unwrap();
    let nudge = element
        .actions
        .iter()
        .find(|spec| spec.id == "nudge")
        .expect("the element's verb");
    let params: Vec<String> = nudge.params().iter().map(|p| p.name.to_string()).collect();
    assert_eq!(params, ["by"]);
    let encoded = nudge
        .build_json(serde_json::json!({ "by": 0.5 }).as_object().unwrap())
        .unwrap();
    let action: <Deck as tilo_resource::Resource>::Action =
        serde_json::from_slice(&encoded.payload).unwrap();
    deck.apply_action(&action).unwrap();
    let state = deck.state().unwrap();
    let x = state
        .slides
        .get(&Id::from_raw("s1"))
        .unwrap()
        .elements
        .get(&Id::from_raw("e1"))
        .unwrap()
        .x;
    assert_eq!(x, 5.0);
}

#[test]
fn a_verb_reads_who_its_invoker_acted_for() {
    let mut board = Replica::<Board>::new().as_agent_for("claude", "quinten");
    board.apply(Board::ADD_CARD_FOR.with(())).unwrap();
    assert_eq!(board.state().unwrap().author, "Agent for quinten");
}

#[test]
fn a_search_entry_lies_in_a_scope_it_names() {
    let entry = tilo_resource::SearchEntry::new("Rust 2.0", "news")
        .anchored("feeds.library/articles/a1")
        .inside("feeds.library/feeds/f1");
    assert_eq!(entry.within.as_deref(), Some("feeds.library/feeds/f1"));
}

#[test]
fn a_verb_prepares_its_arguments_once_where_it_is_invoked() {
    let mut origin = Replica::<Board>::new();
    let mut peer = origin.fork();
    origin.apply(Board::SHOUT.with("sprint")).unwrap();
    assert_eq!(origin.state().unwrap().title, "SPRINT");
    for envelope in origin.log().to_vec() {
        peer.replay(&envelope).unwrap();
    }
    assert_eq!(
        peer.state().unwrap().title,
        "SPRINT",
        "the action carries what was prepared"
    );

    // A flattened struct's absent options are left out.
    origin
        .apply(Board::RESTYLE.with(BoardStyle {
            title: "Plain".into(),
            cards: None,
        }))
        .unwrap();
    assert_eq!(origin.state().unwrap().title, "Plain");
}

/// A field edit sets a value of the field's type: `null` only where the
/// field is optional, never for a number, a flag or a text.
#[test]
fn a_field_edit_refuses_null_for_a_field_that_is_not_optional() {
    use tilo_resource::{Edit, EditOp, Segment};
    let mut board = Replica::<Board>::new();
    for field in ["cards", "title", "layer"] {
        let edit = Edit::<Board>::new(
            vec![Segment::Field(field.into())],
            EditOp::Set {
                value: serde_json::Value::Null,
            },
        );
        assert!(board.apply(edit).is_err(), "`{field}` refuses null");
    }
    board
        .apply(Edit::<Board>::new(
            vec![Segment::Field("cards".into())],
            EditOp::Set {
                value: serde_json::json!(3),
            },
        ))
        .unwrap();
    assert_eq!(board.state().unwrap().cards, 3);
}

#[test]
fn a_nested_item_verb_is_known_offered_without_building_children() {
    use tilo_resource::Resource as _;
    let built = |deck: &Deck| -> Vec<&'static str> {
        let children = Deck::children(deck);
        let mut ids = Vec::new();
        for decl in Deck::declarations() {
            if tilo_resource::items::offered_bound(&children, &decl) {
                ids.push(decl.id);
            }
        }
        ids
    };
    let mut deck = Deck::default();
    assert_eq!(Deck::children_offer_bound(&deck), built(&deck), "no slides");
    deck.slides.push(Id::from_raw("s1"), Slide::default());
    assert_eq!(
        Deck::children_offer_bound(&deck),
        built(&deck),
        "a slide, no elements"
    );
    assert!(Deck::children_offer_bound(&deck).is_empty());
    if let Some(slide) = deck.slides.get_mut(&Id::from_raw("s1")) {
        slide.elements.push(Id::from_raw("e1"), Element::default());
    }
    assert_eq!(
        Deck::children_offer_bound(&deck),
        built(&deck),
        "an element"
    );
    assert_eq!(Deck::children_offer_bound(&deck), ["nudge"]);
}
