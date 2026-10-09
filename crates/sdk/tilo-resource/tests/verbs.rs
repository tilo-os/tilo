//! Verbs: public ids and parameter names, parameter structs with schemas from
//! their types, item verbs, kinds, and the resource options that replace a
//! hand-written `Resource`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test assertions"
)]

use serde::{Deserialize, Serialize};
use tilo_resource::{
    ActionCx, ActionKind, Availability, Collection, Derived, Id, JsonSchema, Model, Outbox, Params,
    Refusal, Replica, Resource, resource,
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
                Derived::new(colour).with(Self::ADD.bind(
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
    let arguments = Shelf::RENAME.arguments(&shelf::Rename { name: "x".into() });
    assert_eq!(arguments["title"], "x");
    assert_eq!(<shelf::Rename as Params>::ACTION.id, "retitle");
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
    assert!(!spec("mail-to").contract.reversible);
    assert!(spec("retitle").contract.reversible);
    assert!(spec(tilo_resource::APPLY).contract.reversible);
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
    shelf.apply_action(&seed, "seed").unwrap();
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
    let add = red.actions.iter().find(|spec| spec.id == "add").unwrap();
    assert_eq!(add.params().len(), 1, "the colour is bound");
    let built = add.build_json(&serde_json::Map::from_iter([(
        "title".to_owned(),
        serde_json::json!("Ruby"),
    )]));
    let encoded = built.unwrap();
    let action: <Shelf as Resource>::Action = serde_json::from_slice(&encoded.payload).unwrap();
    shelf.apply_action(&action, "add").unwrap();
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
