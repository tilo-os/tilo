//! Verbs: public ids and parameter names, parameter structs with schemas from
//! their types, item verbs, kinds, and the resource options that replace a
//! hand-written `Resource`.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test assertions"
)]

use serde::{Deserialize, Serialize};
use tilo_resource::{
    ActionCx, ActionKind, Availability, Collection, Derived, Id, Invoke, JsonSchema, Model, Outbox,
    Params, Refusal, Replica, Resource, resource,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Colour {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    #[model(label)]
    pub title: String,
    pub colour: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub name: String,
    pub cards: Collection<Card>,
    pub archive: Collection<Card>,
    pub mail: Outbox<String>,
    pub seeded: bool,
}

#[resource(
    kind = "test.shelf",
    label = "Shelf",
    params = shelf,
    initialize = Shelf::genesis,
    seed = Shelf::first_content,
    available = Shelf::available,
    children = Shelf::colours,
    read_content = Shelf::content,
)]
impl Shelf {
    /// Rename the shelf.
    #[action(id = "retitle")]
    pub fn rename(&mut self, #[param("The new name", name = "title")] name: String) {
        self.name = name;
    }

    /// Add a card.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String, colour: Option<Colour>) {
        let colour = colour.map_or_else(String::new, |colour| {
            format!("#{:02x}{:02x}{:02x}", colour.red, colour.green, colour.blue)
        });
        self.cards.push(cx.id(), Card { title, colour });
    }

    /// Archive a card.
    #[action(item = cards)]
    pub fn archive_card(&mut self, card: &Id) -> Result<(), Refusal> {
        let taken = self.cards.remove(card).ok_or("no such card")?;
        self.archive.push(card.clone(), taken);
        Ok(())
    }

    /// Restore an archived card.
    #[action]
    pub fn restore(&mut self, archive: &Id) {
        if let Some(card) = self.archive.remove(archive) {
            self.cards.push(archive.clone(), card);
        }
    }

    /// Place a picture on the shelf.
    #[action]
    pub fn place_picture(&mut self, #[param("The picture", content)] picture: String) {
        self.name = format!("{} with {picture}", self.name);
    }

    /// Caption a picture: content another action produced, beside words.
    #[action]
    pub fn caption_picture(
        &mut self,
        #[param("The picture", content)] picture: String,
        #[param("Its caption")] caption: String,
    ) {
        self.name = format!("{} with {picture}: {caption}", self.name);
    }

    /// Mail the shelf to someone.
    #[action]
    pub fn mail_to(&mut self, cx: &mut ActionCx, mail: &mut Outbox<String>, to: String) {
        mail.push(cx.id(), format!("{} to {to}", self.name));
    }

    fn genesis() -> Self {
        Self {
            name: "New shelf".into(),
            ..Self::default()
        }
    }

    fn first_content(&mut self, cx: &mut ActionCx) {
        self.seeded = true;
        self.cards.push(
            cx.id(),
            Card {
                title: "Welcome".into(),
                colour: String::new(),
            },
        );
    }

    fn available(&self, verb: shelf::Verb) -> Availability {
        match verb {
            shelf::Verb::MailTo if self.cards.is_empty() => Availability::Unavailable {
                code: "empty".into(),
                reason: "An empty shelf is not worth mailing".into(),
            },
            _ => Availability::Available,
        }
    }

    fn colours(&self) -> Vec<Derived<Self>> {
        let colours = if self.name.is_empty() {
            Vec::new()
        } else {
            vec!["red", "blue"]
        };
        colours
            .into_iter()
            .map(|colour| {
                Derived::new(colour)
                    .of_kind("colour")
                    .with_child(Derived::new("shade").with(
                        Self::RENAME.bind(shelf::Rename::paths().name, format!("{colour} shelf")),
                    ))
                    .with(Self::ADD.bind(
                        shelf::Add::paths().colour,
                        Some(Colour {
                            red: u8::from(colour == "red") * 255,
                            green: 0,
                            blue: u8::from(colour == "blue") * 255,
                        }),
                    ))
            })
            .collect()
    }

    fn content(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        match item {
            None => Ok(serde_json::json!({ "name": self.name, "cards": self.cards.len() })),
            Some(id) => self
                .cards
                .get(id)
                .map(|card| serde_json::json!({ "title": card.title }))
                .ok_or_else(|| "no such card".to_owned()),
        }
    }
}

fn published(replica: &Replica<Shelf>) -> tilo_actions::Resource {
    tilo_resource::publish::<Shelf>("shelf", "Shelf", &replica.state().unwrap())
}

#[test]
fn ids_and_parameter_names_are_public_names() {
    let mut shelf = Replica::<Shelf>::new();
    assert_eq!(shelf.state().unwrap().name, "New shelf");
    shelf.apply(Shelf::RENAME.with("Kitchen")).unwrap();
    assert_eq!(shelf.state().unwrap().name, "Kitchen");
    let tree = published(&shelf);
    let rename = tree
        .actions
        .iter()
        .find(|spec| spec.id == "retitle")
        .unwrap();
    assert_eq!(rename.params()[0].name, "title");
    assert!(tree.actions.iter().all(|spec| spec.id != "rename"));
    let arguments = Shelf::RENAME
        .arguments(&shelf::Rename { name: "x".into() })
        .unwrap();
    assert_eq!(arguments["title"], "x");
    assert_eq!(<shelf::Rename as Params>::ACTION.id, "retitle");
    // A form reaches the parameters' paths through the trait.
    assert_eq!(
        <shelf::Rename as Params>::paths().name.segments(),
        [tilo_resource::Segment::Field("title".into())]
    );
}

#[test]
fn a_parameter_schema_comes_from_its_type() {
    let schema = <shelf::Add as JsonSchema>::json_schema();
    let text = serde_json::to_string(&schema).unwrap();
    assert!(text.contains("\"red\""), "{text}");
    assert_eq!(schema["required"], serde_json::json!(["title"]));
    let tree = published(&Replica::<Shelf>::new());
    let add = tree.actions.iter().find(|spec| spec.id == "add").unwrap();
    let param = add
        .params()
        .iter()
        .find(|param| param.name == "colour")
        .unwrap();
    assert!(
        serde_json::to_string(param.schema.as_deref().unwrap())
            .unwrap()
            .contains("\"green\"")
    );
    // A colour outside its type is refused when the arguments bind.
    let mut shelf = Replica::<Shelf>::new();
    let mut arguments = Shelf::ADD
        .with(shelf::Add {
            title: "x".into(),
            colour: None,
        })
        .arguments;
    arguments.insert(
        "colour".into(),
        serde_json::json!({ "red": 300, "green": 0, "blue": 0 }),
    );
    assert!(tilo_resource::build_action::<Shelf>("add", &arguments, 0).is_err());
    shelf
        .apply(Shelf::ADD.with((
            "Blue".to_owned(),
            Some(Colour {
                red: 0,
                green: 0,
                blue: 255,
            }),
        )))
        .unwrap();
    assert!(
        shelf
            .state()
            .unwrap()
            .cards
            .values()
            .any(|card| card.colour == "#0000ff")
    );
}

#[test]
fn item_verbs_are_offered_on_the_items_of_their_collection() {
    let mut shelf = Replica::<Shelf>::new();
    shelf
        .apply(Shelf::ADD.with(("Tea".to_owned(), None)))
        .unwrap();
    let id = shelf.state().unwrap().cards.ids().next().unwrap().clone();
    let tree = published(&shelf);
    let card = tree
        .children
        .iter()
        .find(|child| child.id.as_str() == format!("test.shelf/cards/{id}"))
        .unwrap();
    assert_eq!(card.label, "Tea");
    assert!(card.actions.iter().any(|spec| spec.id == "archive-card"));
    assert!(card.actions.iter().all(|spec| spec.id != "restore"));
    shelf.apply(Shelf::ARCHIVE_CARD.with(id.clone())).unwrap();
    let tree = published(&shelf);
    let archived = tree
        .children
        .iter()
        .find(|child| child.id.as_str() == format!("test.shelf/archive/{id}"))
        .unwrap();
    assert!(archived.actions.iter().any(|spec| spec.id == "restore"));
}

#[test]
fn a_verb_with_an_outbox_reaches_outside() {
    let mut shelf = Replica::<Shelf>::new();
    shelf.apply(Shelf::RENAME.with("Pantry")).unwrap();
    shelf
        .apply(Shelf::ADD.with(("Tea".to_owned(), None)))
        .unwrap();
    shelf.apply(Shelf::MAIL_TO.with("ada")).unwrap();
    let state = shelf.state().unwrap();
    assert_eq!(
        state
            .mail
            .pending()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        ["Pantry to ada"]
    );
    let mail = Shelf::MAIL_TO.with("x").build(0).unwrap();
    assert_eq!(Shelf::kind_of(&mail), ActionKind::Outside);
    let rename = Shelf::RENAME.with("x").build(0).unwrap();
    assert_eq!(Shelf::kind_of(&rename), ActionKind::Undoable);
    let tree = published(&shelf);
    let spec = |id: &str| tree.actions.iter().find(|spec| spec.id == id).unwrap();
    // The kind follows from what each action is able to do: a verb that
    // queues reaches outside, one that only edits the document is undoable,
    // and so is the generic `apply`.
    assert_eq!(spec("mail-to").contract.kind, ActionKind::Outside);
    assert_eq!(spec("retitle").contract.kind, ActionKind::Undoable);
    assert_eq!(
        spec(tilo_resource::APPLY).contract.kind,
        ActionKind::Undoable
    );
    assert!(!spec("mail-to").contract.can_be_undone());
    assert!(spec("retitle").contract.can_be_undone());
    assert!(spec(tilo_resource::APPLY).contract.can_be_undone());
    assert_eq!(
        tilo_resource::spec_kind(spec("mail-to")),
        ActionKind::Outside
    );
    assert_eq!(
        tilo_resource::spec_kind(spec("retitle")),
        ActionKind::Undoable
    );
    assert_eq!(
        tilo_resource::spec_kind(spec(tilo_resource::FIELD_EDIT)),
        ActionKind::Undoable
    );
}

#[test]
fn seed_availability_children_and_content() {
    let mut shelf = Replica::<Shelf>::new();
    // Nothing to mail on an empty shelf.
    let tree = published(&shelf);
    let mail = tree
        .actions
        .iter()
        .find(|spec| spec.id == "mail-to")
        .unwrap();
    assert!(!mail.contract.availability.is_available());

    let seed = Shelf::seed().unwrap();
    shelf.apply_action(&seed).unwrap();
    let state = shelf.state().unwrap();
    assert!(state.seeded);
    assert_eq!(state.cards.values().next().unwrap().title, "Welcome");
    let tree = published(&shelf);
    let mail = tree
        .actions
        .iter()
        .find(|spec| spec.id == "mail-to")
        .unwrap();
    assert!(mail.contract.availability.is_available());

    let red = tree
        .children
        .iter()
        .find(|child| child.id.as_str() == "test.shelf/derived/red")
        .unwrap();
    assert_eq!(red.kind, "test.shelf.colour");
    let add = red.actions.iter().find(|spec| spec.id == "add").unwrap();
    assert_eq!(add.params().len(), 1, "the colour is bound");
    let built = add.build_json(&serde_json::Map::from_iter([(
        "title".to_owned(),
        serde_json::json!("Ruby"),
    )]));
    let encoded = built.unwrap();
    let action: <Shelf as Resource>::Action = serde_json::from_slice(&encoded.payload).unwrap();
    shelf.apply_action(&action).unwrap();
    assert!(
        shelf
            .state()
            .unwrap()
            .cards
            .values()
            .any(|card| card.colour == "#ff0000")
    );

    const { assert!(Shelf::READABLE) };
    let content = Shelf::read_content(&shelf.state().unwrap(), shelf.doc(), None).unwrap();
    assert_eq!(content["cards"], 2);
}

#[test]
fn an_input_that_does_not_bind_refuses_where_it_is_submitted() {
    // A hand-written reference whose names do not match its input.
    let wrong: tilo_resource::ActionRef<Shelf, (String,)> =
        tilo_resource::ActionRef::new("add", &["title", "colour"]);
    let invoke = wrong.with(("a".to_owned(),));
    assert!(invoke.refusal().is_some());
    assert!(invoke.arguments.is_empty());
    assert!(wrong.try_with(("a".to_owned(),)).is_err());
    let mut shelf = Replica::<Shelf>::new();
    assert!(shelf.apply(invoke.clone()).is_err());
    assert!(invoke.build(0).is_err());
    assert!(
        shelf
            .apply(tilo_resource::Invoke::group([
                Shelf::RENAME.with("x"),
                invoke
            ]))
            .is_err()
    );
    assert_eq!(shelf.state().unwrap().name, "New shelf");
}

#[test]
fn item_resources_resolve_lazily_as_the_tree_publishes_them() {
    let mut shelf = Replica::<Shelf>::new();
    for title in ["Tea", "Coffee", "Cocoa"] {
        shelf
            .apply(Shelf::ADD.with((title.to_owned(), None)))
            .unwrap();
    }
    let state = shelf.state().unwrap();
    let eager = Shelf::children(&state);
    for child in &eager {
        let lazy = Shelf::child(&state, child.id.as_str()).unwrap();
        assert_eq!(
            tilo_resource::items::fingerprint(&lazy),
            tilo_resource::items::fingerprint(child)
        );
    }
    assert!(Shelf::child(&state, "test.shelf/cards/gone").is_none());
    // Published again, the tree is the same.
    let again = Shelf::children(&state);
    assert_eq!(
        eager
            .iter()
            .map(tilo_resource::items::fingerprint)
            .collect::<Vec<_>>(),
        again
            .iter()
            .map(tilo_resource::items::fingerprint)
            .collect::<Vec<_>>()
    );
    // A renamed item is published under its new label.
    let id = state.cards.ids().next().unwrap().clone();
    shelf
        .apply(Invoke::edit(&Shelf::paths().cards.item(&id).title.set("Green tea".into())).unwrap())
        .unwrap();
    let state = shelf.state().unwrap();
    let renamed = Shelf::child(&state, &format!("test.shelf/cards/{id}")).unwrap();
    assert_eq!(renamed.label, "Green tea");
}

#[test]
fn each_labelled_item_is_its_own_search_entry() {
    let mut shelf = Replica::<Shelf>::new();
    shelf
        .apply(Shelf::ADD.with(("Tea".to_owned(), None)))
        .unwrap();
    let state = shelf.state().unwrap();
    let entries = Shelf::search(&state);
    // The welcome card is not there: no seed was applied.
    assert_eq!(entries.len(), 1);
    let id = state.cards.ids().next().unwrap();
    assert_eq!(entries[0].title, "Tea");
    assert_eq!(
        entries[0].anchor.as_deref(),
        Some(format!("test.shelf/cards/{id}").as_str())
    );
}

#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "test.shelf.view")]
pub enum ShelfView {
    /// Type at the invoker's caret.
    #[effect(edit, participant)]
    TypeHere { text: String },
    /// Scroll to the top.
    #[effect(view)]
    Top,
}

#[test]
fn content_inputs_nested_derived_places_and_edit_effects() {
    use tilo_resource::Effect;
    let shelf = Replica::<Shelf>::new();
    let tree = published(&shelf);
    let place = tree
        .actions
        .iter()
        .find(|spec| spec.id == "place-picture")
        .unwrap();
    assert!(matches!(
        place.input(),
        tilo_actions::ActionInput::Prompted {
            collects: tilo_actions::Collects::Content,
            ..
        }
    ));
    let encoded = place.build_json(&serde_json::Map::from_iter([(
        tilo_actions::PROMPTED_PARAM.to_owned(),
        serde_json::json!("blake3:abc"),
    )]));
    assert!(encoded.is_ok(), "{encoded:?}");

    // Content beside other parameters: every parameter is published, the
    // content one marked as taking what another action produced.
    let caption = tree
        .actions
        .iter()
        .find(|spec| spec.id == "caption-picture")
        .unwrap();
    let params = caption.params();
    let names: Vec<&str> = params.iter().map(|param| &*param.name).collect();
    assert_eq!(names, ["picture", "caption"]);
    assert!(params[0].is_content() && !params[1].is_content());
    let encoded = caption.build_json(&serde_json::Map::from_iter([
        ("picture".to_owned(), serde_json::json!("abc")),
        ("caption".to_owned(), serde_json::json!("Sunset")),
    ]));
    assert!(encoded.is_ok(), "{encoded:?}");

    let shade = Shelf::child(&shelf.state().unwrap(), "test.shelf/derived/red/shade").unwrap();
    assert!(shade.actions.iter().any(|spec| spec.id == "retitle"));

    let kinds: Vec<ActionKind> = ShelfView::specs()
        .iter()
        .map(tilo_resource::spec_kind)
        .collect();
    assert_eq!(kinds, [ActionKind::Undoable, ActionKind::Read]);
}

/// A card's id, as the app names it.
pub type CardKey = uuid::Uuid;

/// Something with no schema of its own.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Opaque {
    pub raw: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "test.shelf.cards")]
pub enum CardEffect {
    /// Show a card.
    Show {
        card: CardKey,
        colour: Option<Colour>,
        opaque: Opaque,
    },
}

#[test]
fn an_effect_parameter_publishes_what_its_type_is_written_as() {
    use tilo_resource::Effect as _;
    let spec = CardEffect::specs()
        .into_iter()
        .find(|spec| spec.id == "show")
        .unwrap();
    let kinds: Vec<(String, tilo_actions::ParamType)> = spec
        .params()
        .iter()
        .map(|param| (param.name.to_string(), param.ty))
        .collect();
    assert_eq!(
        kinds,
        [
            ("card".to_owned(), tilo_actions::ParamType::Text),
            ("colour".to_owned(), tilo_actions::ParamType::Object),
            ("opaque".to_owned(), tilo_actions::ParamType::Object),
        ]
    );
}

/// A tag a test host treats an effect by.
const PRINTED_TAG: &str = "test:printed";

#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "test.shelf.press")]
pub enum PressEffect {
    /// Print the shelf.
    #[effect(job, returns, host_render, tag = PRINTED_TAG)]
    Print,
    /// Count the cards.
    #[effect(returns)]
    Count,
    /// Export the shelf.
    #[effect(returns, kind = "read", outside_with = "path")]
    Export {
        #[param("Where to write it; kept as content when absent")]
        path: Option<String>,
    },
}

#[test]
fn an_export_reaches_outside_only_when_told_where_to_write() {
    use tilo_resource::Effect as _;
    let export = PressEffect::specs()
        .into_iter()
        .find(|spec| spec.id == "export")
        .unwrap();
    assert_eq!(
        export.contract.kind,
        ActionKind::Read,
        "offered as the least it does"
    );
    let need = |arguments: serde_json::Value| {
        export
            .need_for(Some(arguments.as_object().unwrap()), None)
            .kind
    };
    assert_eq!(need(serde_json::json!({})), ActionKind::Read);
    assert_eq!(need(serde_json::json!({ "path": null })), ActionKind::Read);
    assert_eq!(
        need(serde_json::json!({ "path": "/tmp/shelf.json" })),
        ActionKind::Outside
    );
}

#[test]
fn an_effect_carries_the_tags_its_host_treats_it_by() {
    use tilo_resource::Effect as _;
    let specs = PressEffect::specs();
    let tags = |id: &str| {
        specs
            .iter()
            .find(|spec| spec.id == id)
            .unwrap()
            .contract
            .tags
            .clone()
    };
    let print = tags("print");
    assert!(
        print.iter().any(|tag| tag == tilo_actions::HOST_RENDER_TAG),
        "a host-rendered effect says so: {print:?}"
    );
    assert!(print.iter().any(|tag| tag == PRINTED_TAG), "{print:?}");
    let count = tags("count");
    assert!(
        count
            .iter()
            .all(|tag| tag != tilo_actions::HOST_RENDER_TAG && tag != PRINTED_TAG),
        "an untagged effect carries neither: {count:?}"
    );
}

/// A card in an address book: named by first and last name, else its
/// organization.
#[derive(tilo_resource::Model, Default, Debug, Clone, PartialEq)]
#[model(label = "first last")]
pub struct Person {
    pub first: String,
    pub last: String,
    #[model(label)]
    pub organization: String,
}

#[derive(tilo_resource::Model, Default, Debug, Clone, PartialEq)]
pub struct Book {
    pub people: Collection<Person>,
}

#[resource(kind = "test.book", label = "Book", params = book)]
impl Book {
    /// Add a card.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, first: String, last: String, organization: String) {
        self.people.push(
            cx.id(),
            Person {
                first,
                last,
                organization,
            },
        );
    }

    /// Remove a card.
    #[action]
    pub fn remove(&mut self, people: &Id) {
        self.people.remove(people);
    }

    /// Change a card's first name.
    #[action(item = people)]
    pub fn rename(person: &mut Person, cx: &mut ActionCx, first: String) {
        let _ = cx.now_ms();
        person.first = first;
    }
}

#[test]
fn an_item_label_is_made_of_several_fields() {
    let mut book = Replica::<Book>::new();
    book.apply(Book::ADD.with(("Ada".to_owned(), "Lovelace".to_owned(), String::new())))
        .unwrap();
    book.apply(Book::ADD.with((
        String::new(),
        String::new(),
        "Analytical Engines".to_owned(),
    )))
    .unwrap();
    let state = book.state().unwrap();
    let labels: Vec<Option<String>> = state.people.values().map(Model::label).collect();
    assert_eq!(
        labels,
        [
            Some("Ada Lovelace".to_owned()),
            Some("Analytical Engines".to_owned())
        ]
    );
    let tree = book.published().unwrap();
    let mut items: Vec<&str> = tree
        .children
        .iter()
        .map(|child| child.label.as_str())
        .collect();
    items.sort_unstable();
    assert_eq!(items, ["Ada Lovelace", "Analytical Engines"]);

    // History names the item the same way.
    let (ada, _) = state.people.iter().next().unwrap();
    let edit = Book::edit_action(Book::paths().people.item(ada).last.replace("Byron")).unwrap();
    let entry = book.apply_action(&edit).unwrap().context.clone().unwrap();
    assert_eq!(entry.item.unwrap().label, "Ada Lovelace");
}

/// A report, kept under the id of the app it is about.
#[derive(tilo_resource::Model, Default, Debug, Clone, PartialEq)]
pub struct Report {
    #[model(label)]
    pub title: String,
    pub note: String,
}

#[derive(tilo_resource::Model, Default, Debug, Clone, PartialEq)]
pub struct Reports {
    pub reports: std::collections::BTreeMap<String, Report>,
    pub tags: tilo_resource::Set<String>,
}

#[resource(kind = "test.reports", label = "Reports", params = reports)]
impl Reports {
    /// File a report.
    #[action]
    pub fn file(&mut self, app: String, title: String) {
        self.reports.insert(
            app,
            Report {
                title,
                note: String::new(),
            },
        );
    }
}

fn described(
    replica: &mut Replica<Reports>,
    edit: tilo_resource::Edit<Reports>,
) -> tilo_actions::Described {
    let action = Reports::edit_action(edit).unwrap();
    replica
        .apply_action(&action)
        .unwrap()
        .context
        .clone()
        .unwrap()
        .described()
        .unwrap()
}

#[test]
fn an_edit_under_a_map_key_is_about_that_entry_and_a_set_names_its_element() {
    let mut reports = Replica::<Reports>::new();
    reports
        .apply(Reports::FILE.with(("so.tilo.notes".to_owned(), "Notes crash".to_owned())))
        .unwrap();
    let note = described(
        &mut reports,
        Reports::paths()
            .reports
            .key("so.tilo.notes")
            .note
            .replace("on save"),
    );
    let tilo_actions::Did::Edit(field) = &note.what else {
        panic!("a field edit");
    };
    assert_eq!(
        (field.ty.as_str(), field.field.as_str()),
        ("report", "note")
    );
    let item = note.item.unwrap();
    assert_eq!(
        (item.id.as_str(), item.label.as_str()),
        ("so.tilo.notes", "Notes crash")
    );

    let removed = described(
        &mut reports,
        Reports::paths().reports.remove("so.tilo.notes"),
    );
    let item = removed.item.unwrap();
    assert_eq!(
        (item.id.as_str(), item.label.as_str()),
        ("so.tilo.notes", "Notes crash")
    );

    let tagged = described(
        &mut reports,
        Reports::paths().tags.insert(&"urgent".to_owned()),
    );
    let tilo_actions::Did::Edit(field) = &tagged.what else {
        panic!("a field edit");
    };
    assert_eq!(
        field.new.as_ref().and_then(|new| new.as_str()),
        Some("urgent")
    );
}

#[test]
fn an_item_method_reads_and_writes_its_item_alone() {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ReadDoc as _, ScalarValue, Value};
    let mut book = Replica::<Book>::new();
    book.apply(Book::ADD.with(("Ada".to_owned(), "Lovelace".to_owned(), String::new())))
        .unwrap();
    book.apply(Book::ADD.with(("Charles".to_owned(), "Babbage".to_owned(), String::new())))
        .unwrap();
    let state = book.state().unwrap();
    let ids: Vec<Id> = state.people.ids().cloned().collect();

    // It is offered on each card, its id bound.
    let tree = book.published().unwrap();
    assert!(
        tree.children
            .iter()
            .all(|card| card.actions.iter().any(|spec| spec.id == "rename"))
    );

    // Another card that no longer reads (written by a newer version) is
    // quarantined, and does not stop a change of this one: only this one is
    // read.
    let doc = book.doc_mut();
    let Ok(Some((Value::Object(ObjType::Map), people))) = doc.get(automerge::ROOT, "people") else {
        panic!("people is a map");
    };
    let Ok(Some((Value::Object(ObjType::Map), other))) = doc.get(&people, ids[1].as_str()) else {
        panic!("the card is there");
    };
    doc.put(&other, "v", ScalarValue::Str("from the future".into()))
        .unwrap();
    doc.commit();
    let held: Vec<Id> = book
        .state()
        .unwrap()
        .people
        .quarantined()
        .iter()
        .map(|held| held.id.clone())
        .collect();
    assert_eq!(held, [ids[1].clone()], "only that card is held aside");
    let rename = Book::RENAME
        .with((ids[0].clone(), "Augusta".to_owned()))
        .build(0)
        .unwrap();
    tilo_actions::Action::apply(&rename, book.doc_mut()).unwrap();
    let first = book.doc().get(&people, ids[0].as_str()).unwrap().unwrap().1;
    let value = book.doc().get(&first, "v").unwrap().unwrap().1;
    let person: Person = tilo_resource::read_key(book.doc(), &first, "v").unwrap();
    let _ = value;
    assert_eq!(person.first, "Augusta");
    assert_eq!(person.last, "Lovelace");
}

/// A hit opened by a view effect of a live row is anchored at the row and
/// runs that effect there.
#[test]
fn a_search_entry_opens_by_its_rows_effect() {
    let entry =
        tilo_resource::SearchEntry::new("Chapter 2", "").opening("shelf.top:2", &ShelfView::Top);
    assert_eq!(entry.anchor.as_deref(), Some("shelf.top:2"));
    let action = entry.action.expect("the row's effect");
    assert_eq!(action.resource, "shelf.top:2");
    assert_eq!(action.action, "top");
}

#[test]
fn an_edit_inside_a_missing_map_entry_starts_the_entry() {
    let mut reports = Replica::<Reports>::new();
    let action = Reports::edit_action(
        Reports::paths()
            .reports
            .key("so.tilo.maps")
            .note
            .replace("needs a key"),
    )
    .unwrap();
    reports.apply_action(&action).unwrap();
    let state = reports.state().unwrap();
    let report = state.reports.get("so.tilo.maps").unwrap();
    assert_eq!(report.note, "needs a key");
    assert_eq!(report.title, "");
}

/// Pages whose places are published only once a view loaded them.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Pages {
    pub titles: Vec<String>,
    #[model(skip)]
    pub loaded: Vec<usize>,
}

#[resource(kind = "test.pages", label = "Pages", load = Pages::load_pages, children = Pages::places)]
impl Pages {
    /// Add a page.
    #[action]
    pub fn add_page(&mut self, title: String) {
        self.titles.push(title);
    }

    #[allow(clippy::unnecessary_wraps, reason = "the load hook's signature")]
    fn load_pages(
        &mut self,
        _doc: &tilo_resource::Doc,
        demand: &tilo_resource::Demand,
    ) -> Result<(), tilo_resource::ModelError> {
        self.loaded = demand
            .keys
            .iter()
            .filter_map(|key| key.strip_prefix('p')?.parse().ok())
            .filter(|index| *index < self.titles.len())
            .collect();
        Ok(())
    }

    fn places(&self) -> Vec<Derived<Self>> {
        self.loaded
            .iter()
            .map(|&index| {
                Derived::new(self.titles[index].clone())
                    .identified(format!("p{index}"))
                    .in_field("pages")
            })
            .collect()
    }
}

#[test]
fn a_place_not_loaded_still_resolves_by_id() {
    let mut pages = Replica::<Pages>::new();
    for title in ["Cover", "Contents", "Chapter one"] {
        pages.apply(Pages::ADD_PAGE.with(title.to_owned())).unwrap();
    }
    let state = Pages::read(pages.doc()).unwrap();
    let id = "test.pages/pages/p2";
    assert!(Pages::child(&state, id).is_none(), "nothing is loaded");
    let child = tilo_resource::child_by_id::<Pages>(&state, pages.doc(), id).expect("resolved");
    assert_eq!(child.label, "Chapter one");
}

fn three_pages() -> Replica<Pages> {
    let mut pages = Replica::<Pages>::new();
    for title in ["Cover", "Contents", "Chapter one"] {
        pages.apply(Pages::ADD_PAGE.with(title.to_owned())).unwrap();
    }
    pages
}

#[test]
fn places_as_they_were_are_the_publication_they_were() {
    let pages = three_pages();
    let mut state = Pages::read(pages.doc()).unwrap();
    state.loaded = vec![0, 1];
    let first = tilo_resource::derived::published::<Pages>(state.places());
    let again = tilo_resource::derived::published::<Pages>(state.places());
    assert!(std::rc::Rc::ptr_eq(&first, &again));

    state.loaded = vec![0, 1, 2];
    let more = tilo_resource::derived::published::<Pages>(state.places());
    assert!(!std::rc::Rc::ptr_eq(&first, &more));
    let labels: Vec<&str> = more
        .resources()
        .iter()
        .map(|place| place.label.as_str())
        .collect();
    assert_eq!(labels, ["Cover", "Contents", "Chapter one"]);
    for (was, now) in first.resources().iter().zip(more.resources().iter()) {
        assert!(
            std::rc::Rc::ptr_eq(was, now),
            "{} is published again",
            was.label
        );
    }
}

#[test]
fn reading_more_of_a_document_is_no_revision() {
    let mut pages = three_pages();
    let mut reader = tilo_resource::Reader::<Pages>::open(pages.doc_mut()).unwrap();
    let revision = reader.revision();
    let demand = tilo_resource::Demand::none().with_keys(["p0", "p2"]);
    assert!(reader.demand(pages.doc(), demand).unwrap());
    assert_eq!(reader.state().loaded, [0, 2]);
    assert_eq!(reader.revision(), revision);
    assert_eq!(reader.loads(), 1);

    pages
        .apply(Pages::ADD_PAGE.with("Index".to_owned()))
        .unwrap();
    assert!(reader.sync(pages.doc()).unwrap());
    assert_ne!(reader.revision(), revision);
}

/// An absent option given positionally is left out, as a parameter not
/// given, rather than sent as `null` the door refuses.
#[test]
fn an_absent_positional_option_is_not_sent() {
    let add: tilo_resource::ActionRef<Shelf, (String, Option<Colour>)> =
        tilo_resource::ActionRef::new("add", &["title", "colour"]);
    let arguments = add.arguments(&("Tea".to_owned(), None)).unwrap();
    assert_eq!(arguments.len(), 1);
    assert_eq!(arguments["title"], "Tea");
    let single: tilo_resource::ActionRef<Shelf, Option<String>> =
        tilo_resource::ActionRef::new("retitle", &["title"]);
    assert!(single.arguments(&None).unwrap().is_empty());
    let mut shelf = Replica::<Shelf>::new();
    shelf.apply(add.with(("Tea".to_owned(), None))).unwrap();
    assert_eq!(shelf.state().unwrap().cards.len(), 1);
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PrivateImport {
    pub received_bytes: u64,
}

#[resource(kind = "test.private-import", label = "Private import")]
impl PrivateImport {
    /// Receive a private snapshot.
    #[action]
    pub fn import(&mut self, #[param(secret, "Private snapshot")] payload: String) {
        self.received_bytes = payload.len() as u64;
    }

    /// Receive an optional private snapshot.
    #[action]
    pub fn optional_import(
        &mut self,
        #[param("Private snapshot", secret)] payload: Option<String>,
    ) {
        self.received_bytes = payload.map_or(0, |payload| payload.len() as u64);
    }
}

#[test]
fn typed_secret_parameters_bind_redacted_variants_and_refuse_plaintext_arguments() {
    use tilo_actions::{ActionArgs, ArgValue, ParamType};
    let mut record = Replica::<PrivateImport>::new();
    let tree = tilo_resource::publish::<PrivateImport>(
        "private",
        "Private import",
        &record.state().unwrap(),
    );
    let action = tree
        .actions
        .iter()
        .find(|action| action.id == "import")
        .unwrap();
    let params = action.params();
    assert_eq!(params[0].ty, ParamType::Secret);
    assert!(params[0].required);
    assert!(params[0].choices.is_empty());
    let supplied = serde_json::json!({"payload":"private-history-sentinel"});
    let bound = ActionArgs::bind_json(params, supplied.as_object().unwrap()).unwrap();
    let debug = format!("{bound:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("private-history-sentinel"));
    assert!(
        ActionArgs::bind(
            params,
            [(
                "payload".to_owned(),
                ArgValue::Text("private-history-sentinel".into())
            )]
        )
        .is_err()
    );
    assert!(
        ActionArgs::bind_json(
            params,
            serde_json::json!({"payload":37}).as_object().unwrap()
        )
        .is_err()
    );
    record
        .apply(PrivateImport::IMPORT.with("private-history-sentinel".to_owned()))
        .unwrap();
    assert_eq!(record.state().unwrap().received_bytes, 24);
    let optional = tree
        .actions
        .iter()
        .find(|action| action.id == "optional-import")
        .unwrap();
    assert_eq!(optional.params()[0].ty, ParamType::Secret);
    assert!(!optional.params()[0].required);
    assert!(ActionArgs::bind_json(optional.params(), &serde_json::Map::new()).is_ok());
}
