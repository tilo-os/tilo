//! The deck's typed operations, driven the way an agent drives them: through
//! the tree the deck publishes (and resolves by id) and the shared action
//! door, against a deck held as a headless participant holds it.

#![allow(
    clippy::float_cmp,
    reason = "every compared number is a literal the test itself passed in, stored unchanged"
)]

use automerge::{ActorId, AutoCommit};
use serde_json::{Value, json};
use tilo_actions::{ActionAddress, ActionRegistry, DoorError, LocalTransport, Resource};

use crate::actions::{
    self, ADD_ELEMENT, ADD_SLIDE, DELETE_ELEMENT, MOVE_ELEMENT, MOVE_SLIDE, RESTYLE_ELEMENT,
    SlidesResourceCtx,
};
use crate::doc::SlidesDoc;
use crate::model::{Deck, ElementKind, TextAlign, seed_deck};

const AGENT: &[u8] = b"deck-operations-agent";

struct Participant {
    doc: AutoCommit,
    registry: ActionRegistry,
}

impl Participant {
    fn new() -> Self {
        let mut doc = AutoCommit::new();
        SlidesDoc::seed(&mut doc, &seed_deck());
        doc.commit();
        doc.set_actor(ActorId::from(AGENT));
        let registry = ActionRegistry::with(actions::register);
        Self { doc, registry }
    }

    fn deck(&self) -> Deck {
        SlidesDoc::load(&self.doc).expect("the deck loads")
    }

    fn resources(&self) -> Vec<Resource> {
        vec![published(&SlidesResourceCtx::from_deck(&self.deck()))]
    }

    fn invoke(&mut self, action: &str, args: &Value) -> Result<(), DoorError> {
        self.invoke_on("slides.deck", action, args, false)
    }

    fn invoke_on(
        &mut self,
        resource: &str,
        action: &str,
        args: &Value,
        confirmed: bool,
    ) -> Result<(), DoorError> {
        let resources = self.resources();
        let address = ActionAddress::new(resource, action);
        let args = args.as_object().expect("arguments are an object");
        let mut transport = LocalTransport::default();
        let result = if confirmed {
            tilo_actions::door::invoke_json_confirmed(
                &mut self.doc,
                &resources,
                &address,
                args,
                tilo_actions::door::InvocationProvenance::caller(AGENT),
                &self.registry,
                &mut transport,
            )
        } else {
            tilo_actions::door::invoke_json(
                &mut self.doc,
                &resources,
                &address,
                args,
                tilo_actions::door::InvocationProvenance::caller(AGENT),
                &self.registry,
                &mut transport,
            )
        };
        result.map(|_| ())
    }
}

/// The deck as its resource publishes it.
fn published(ctx: &SlidesResourceCtx) -> Resource {
    ui::tilo_resource::publish::<actions::SlidesDeck>("slides.deck", "Deck", ctx)
}

/// The published resource `id`, resolved by id as an agent's request is.
fn resolved(ctx: &SlidesResourceCtx, id: &str) -> Option<Resource> {
    <actions::SlidesDeck as ui::tilo_resource::Resource>::child(ctx, id)
}

fn action_ids(resource: &Resource) -> Vec<&str> {
    resource.actions.iter().map(|a| a.id.as_str()).collect()
}

#[test]
fn the_deck_publishes_its_typed_operations_eagerly_and_by_id() {
    let participant = Participant::new();
    let ctx = SlidesResourceCtx::from_deck(&participant.deck());
    let eager = vec![published(&ctx)];
    {
        let deck = &eager[0];
        let ids = action_ids(deck);
        for id in [
            ADD_ELEMENT,
            MOVE_ELEMENT,
            RESTYLE_ELEMENT,
            DELETE_ELEMENT,
            MOVE_SLIDE,
            ui::tilo_resource::APPLY,
            ui::tilo_resource::GROUP,
            // Bound document ops ride publish via bound_actions.
            ADD_SLIDE,
            "theme-emerald",
        ] {
            assert!(ids.contains(&id), "`{id}` is published: {ids:?}");
        }
        assert_eq!(deck.children.len(), seed_deck().slides.len());
    }
    let add = eager[0]
        .actions
        .iter()
        .find(|a| a.id == ADD_ELEMENT)
        .unwrap();
    let params: Vec<&str> = add.params().iter().map(|p| p.name.as_ref()).collect();
    assert_eq!(
        params,
        [
            "slide",
            "kind",
            "x",
            "y",
            "w",
            "h",
            "text",
            "src",
            "fill",
            "text_color",
            "stroke",
            "stroke_width",
            "font_size",
            "align",
            "bold",
            "italic",
            "radius",
        ]
    );
    let violations = tilo_actions::validate_surface_contracts(&eager, &participant.registry);
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn add_element_creates_it_on_top_with_a_minted_id() {
    let mut participant = Participant::new();
    let before = participant.deck();
    let slide = &before.slides[4];
    let top = slide.elements.iter().map(|e| e.z).max().unwrap();
    participant
        .invoke(
            ADD_ELEMENT,
            &json!({
                "slide": actions::slide_resource_id(&slide.id),
                "kind": "text",
                "x": 110, "y": 540, "w": 900, "h": 60,
                "text": "hello@example.com",
                "text_color": "text-dim",
                "font_size": 28,
                "align": "center",
                "italic": true,
            }),
        )
        .expect("add-element applies");
    let after = participant.deck();
    let added: Vec<_> = after.slides[4]
        .elements
        .iter()
        .filter(|e| slide.element(&e.id).is_none())
        .collect();
    assert_eq!(added.len(), 1, "exactly one element is added");
    let added = added[0];
    assert_eq!(added.kind, ElementKind::Text);
    assert_eq!(added.text, "hello@example.com");
    assert_eq!(added.fill, "text-dim");
    assert_eq!(added.font_size, 28.0);
    assert_eq!(added.align, TextAlign::Center);
    assert!(added.italic && !added.bold);
    assert_eq!(
        (added.x, added.y, added.w, added.h),
        (110.0, 540.0, 900.0, 60.0)
    );
    assert_eq!(added.z, top + 1, "the new element is above everything");
    assert_eq!(added.id.len(), 20, "the id comes from the action's context");
    // The element is addressable in the tree that is published next.
    assert!(
        resolved(
            &SlidesResourceCtx::from_deck(&after),
            &actions::element_resource_id(&added.id),
        )
        .is_some()
    );
}

#[test]
fn add_element_refuses_what_the_inspector_could_not_make() {
    let mut participant = Participant::new();
    let slide = participant.deck().slides[0].id.clone();
    let heads = participant.doc.get_heads();
    for args in [
        json!({"slide": slide, "kind": "star", "x": 0, "y": 0, "w": 10, "h": 10}),
        json!({"slide": slide, "kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10, "fill": "plaid"}),
        json!({"slide": slide, "kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10, "text": "x"}),
        json!({"slide": slide, "kind": "image", "x": 0, "y": 0, "w": 10, "h": 10}),
        json!({"slide": slide, "kind": "text", "x": 0, "y": 0, "w": 10, "h": 10,
               "fill": "accent", "text_color": "white"}),
        json!({"slide": "no-such-slide", "kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10}),
        json!({"slide": slide, "kind": "rect", "x": 0, "y": 0, "w": 0, "h": 10}),
    ] {
        assert!(
            participant.invoke(ADD_ELEMENT, &args).is_err(),
            "refused: {args}"
        );
        assert_eq!(
            participant.doc.get_heads(),
            heads,
            "nothing written: {args}"
        );
    }
}

#[test]
fn move_element_moves_and_resizes_by_id() {
    let mut participant = Participant::new();
    let target = participant.deck().slides[2].elements[3].clone();
    participant
        .invoke(
            MOVE_ELEMENT,
            &json!({"element": target.id, "x": 140, "y": 260}),
        )
        .expect("move applies");
    let moved = participant.deck().slides[2]
        .element(&target.id)
        .cloned()
        .unwrap();
    assert_eq!(
        (moved.x, moved.y, moved.w, moved.h),
        (140.0, 260.0, target.w, target.h)
    );

    participant
        .invoke(
            MOVE_ELEMENT,
            &json!({"element": actions::element_resource_id(&target.id),
                    "x": 150, "y": 270, "w": 400, "h": 40}),
        )
        .expect("resize applies");
    let resized = participant.deck().slides[2]
        .element(&target.id)
        .cloned()
        .unwrap();
    assert_eq!(
        (resized.x, resized.y, resized.w, resized.h),
        (150.0, 270.0, 400.0, 40.0)
    );

    assert!(
        participant
            .invoke(
                MOVE_ELEMENT,
                &json!({"element": target.id, "x": 1, "y": 1, "w": 5})
            )
            .is_err(),
        "a width without a height is refused"
    );
}

#[test]
fn restyle_element_changes_only_the_named_styles() {
    let mut participant = Participant::new();
    // The closing slide's subtitle: the text that must stay legible.
    let target = participant.deck().slides[4].elements[2].clone();
    participant
        .invoke(
            RESTYLE_ELEMENT,
            &json!({"element": target.id, "text_color": "white", "bold": true,
                    "font_size": 36, "align": "right"}),
        )
        .expect("restyle applies");
    let styled = participant.deck().slides[4]
        .element(&target.id)
        .cloned()
        .unwrap();
    assert_eq!(styled.fill, "white");
    assert!(styled.bold);
    assert_eq!(styled.font_size, 36.0);
    assert_eq!(styled.align, TextAlign::Right);
    assert_eq!(styled.italic, target.italic);
    assert_eq!(styled.stroke, target.stroke);
    assert_eq!((styled.x, styled.y), (target.x, target.y));

    assert!(
        participant
            .invoke(RESTYLE_ELEMENT, &json!({"element": target.id}))
            .is_err(),
        "a restyle that names no style is refused"
    );
}

#[test]
fn delete_element_is_destructive_and_needs_confirmation() {
    let mut participant = Participant::new();
    let target = participant.deck().slides[1].elements[0].id.clone();
    assert!(matches!(
        participant.invoke(DELETE_ELEMENT, &json!({"element": target})),
        Err(DoorError::ConfirmationRequired)
    ));
    assert!(participant.deck().slides[1].element(&target).is_some());
    participant
        .invoke_on(
            "slides.deck",
            DELETE_ELEMENT,
            &json!({"element": target}),
            true,
        )
        .expect("a confirmed delete applies");
    assert!(participant.deck().slides[1].element(&target).is_none());
}

#[test]
fn move_slide_reorders_the_deck() {
    let mut participant = Participant::new();
    let order: Vec<String> = participant
        .deck()
        .slides
        .iter()
        .map(|s| s.id.clone())
        .collect();
    participant
        .invoke(MOVE_SLIDE, &json!({"slide": order[4], "position": 1}))
        .expect("move-slide applies");
    let after: Vec<String> = participant
        .deck()
        .slides
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(
        after,
        [&order[0], &order[4], &order[1], &order[2], &order[3]].map(String::clone)
    );
    participant
        .invoke(MOVE_SLIDE, &json!({"slide": order[4], "position": 4}))
        .expect("and back to the end");
    let back: Vec<String> = participant
        .deck()
        .slides
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(back, order);
    assert!(
        participant
            .invoke(MOVE_SLIDE, &json!({"slide": order[0], "position": 9}))
            .is_err(),
        "a position past the end is refused"
    );
}

#[test]
fn a_group_applies_several_operations_as_one_change() {
    let mut participant = Participant::new();
    let deck = participant.deck();
    let slide = deck.slides[3].id.clone();
    let add = json!({"slide": slide, "kind": "rect", "x": 96, "y": 600, "w": 300, "h": 40});
    let changes = participant.doc.get_changes(&[]).len();
    participant
        .invoke(
            ui::tilo_resource::GROUP,
            &json!({"actions": [
                {"action": ADD_ELEMENT, "args": add},
                {"action": ADD_ELEMENT, "args": add},
                {"action": MOVE_SLIDE, "args": {"slide": slide, "position": 0}},
            ]}),
        )
        .expect("the group applies");
    assert_eq!(
        participant.doc.get_changes(&[]).len(),
        changes + 1,
        "one change"
    );
    let after = participant.deck();
    assert_eq!(after.slides[0].id, slide);
    let added: Vec<_> = after.slides[0]
        .elements
        .iter()
        .filter(|e| deck.slides[3].element(&e.id).is_none())
        .collect();
    assert_eq!(added.len(), 2, "two identical requests add two elements");
    assert_ne!(added[0].id, added[1].id, "each member mints its own id");
}

#[test]
fn apply_takes_any_slides_action_as_json() {
    let mut participant = Participant::new();
    let slide = participant.deck().slides[1].id.clone();
    participant
        .invoke(
            ui::tilo_resource::APPLY,
            &json!({"action": {"SetSlideBackground": {
                "slide": 1, "id": slide, "background": "deep"}}}),
        )
        .expect("apply applies");
    assert_eq!(
        participant.deck().slides[1].background,
        crate::model::Background::Deep
    );
    // A destructive value asks for confirmation through `apply` as well.
    let element = participant.deck().slides[1].elements[0].id.clone();
    assert!(matches!(
        participant.invoke(
            ui::tilo_resource::APPLY,
            &json!({"action": {"DeleteElement": {"slide": 1, "id": element}}}),
        ),
        Err(DoorError::ConfirmationRequired)
    ));
}

#[test]
fn a_created_element_is_the_same_on_two_replicas() {
    let origin = Participant::new();
    let slide = origin.deck().slides[0].id.clone();
    let encoded = tilo_actions::door::encode_json(
        &origin.resources(),
        &ActionAddress::new("slides.deck", ADD_ELEMENT),
        json!({"slide": slide, "kind": "ellipse", "x": 900, "y": 100, "w": 200, "h": 200})
            .as_object()
            .unwrap(),
    )
    .unwrap();

    let bytes = origin.doc.clone().save();
    let mut a = AutoCommit::load(&bytes).unwrap();
    let mut b = AutoCommit::load(&bytes).unwrap();
    a.set_actor(ActorId::from(b"replica-a".as_slice()));
    b.set_actor(ActorId::from(b"replica-b".as_slice()));
    let action = origin.registry.decode(&encoded).unwrap();
    let envelope = tilo_actions::apply_local(&mut a, action.as_ref(), encoded.clone()).unwrap();
    tilo_actions::replay(&mut b, &envelope, &origin.registry).unwrap();
    assert_eq!(a.get_heads(), b.get_heads(), "the same change on both");
    let (left, right) = (SlidesDoc::load(&a).unwrap(), SlidesDoc::load(&b).unwrap());
    assert_eq!(left, right);
    let minted = left.slides[0]
        .elements
        .iter()
        .filter(|e| origin.deck().slides[0].element(&e.id).is_none())
        .count();
    assert_eq!(minted, 1);

    // The same request authored again, on top of the first, is a new element.
    let again = origin.registry.decode(&encoded).unwrap();
    tilo_actions::apply_local(&mut a, again.as_ref(), encoded).unwrap();
    let ids: std::collections::BTreeSet<_> = SlidesDoc::load(&a).unwrap().slides[0]
        .elements
        .iter()
        .map(|e| e.id.clone())
        .collect();
    assert_eq!(ids.len(), origin.deck().slides[0].elements.len() + 2);
}

/// Recorded history keeps replaying: every payload shape written before the
/// typed operations existed decodes to the same action and encodes back to
/// the same bytes.
#[test]
fn recorded_payloads_decode_and_encode_unchanged() {
    for payload in [
        r#"{"MoveElement":{"slide":1,"id":"e1","x":10.0,"y":20.0}}"#,
        r#"{"ResizeElement":{"slide":0,"id":"e1","x":1.0,"y":2.0,"w":3.0,"h":4.0}}"#,
        r#"{"RestyleElement":{"slide":0,"id":"e1","patch":{"fill":"accent","stroke":null,"align":null,"font_size":40.0,"stroke_width":null,"radius":null,"bold":true,"italic":null}}}"#,
        r#"{"DeleteElement":{"slide":2,"id":"e1"}}"#,
        r#"{"MoveSlide":{"from":1,"to":3,"id":"s1"}}"#,
        r#"{"SetSlideOrder":{"slide_id":"s1","rank":42}}"#,
        r#"{"SetElementZ":{"slide_id":"s1","element_id":"e1","z":-3}}"#,
        r#"{"SetSlideBackground":{"slide":0,"id":"s1","background":"deep"}}"#,
        r#"{"SetDeckPalette":{"palette":"rose"}}"#,
    ] {
        let action: actions::SlidesAction = serde_json::from_str(payload).expect(payload);
        assert_eq!(serde_json::to_string(&action).unwrap(), payload);
    }
}

#[test]
fn add_slide_writes_its_title_and_body_in_one_step() {
    let mut participant = Participant::new();
    let before = participant.deck().slides.len();
    participant
        .invoke(
            "add-slide",
            &json!({"layout": "title", "title": "Product Launch", "body": "October 2026"}),
        )
        .unwrap();
    participant
        .invoke(
            "add-slide",
            &json!({"title": "Agenda", "body": "Goals\nTimeline\nBudget"}),
        )
        .unwrap();
    participant.invoke("add-slide", &json!({})).unwrap();
    let deck = participant.deck();
    assert_eq!(deck.slides.len(), before + 3);
    let texts = |index: usize| -> Vec<String> {
        deck.slides[index]
            .elements
            .iter()
            .filter(|element| element.kind == ElementKind::Text)
            .map(|element| element.text.clone())
            .collect()
    };
    assert_eq!(texts(before), ["Product Launch", "October 2026"]);
    assert_eq!(deck.slides[before].layout, crate::model::Layout::Title);
    assert_eq!(texts(before + 1), ["Agenda", "Goals\nTimeline\nBudget"]);
    assert_eq!(
        texts(before + 2),
        ["New slide", "Add your content…"],
        "no arguments: the template"
    );
    assert!(
        participant
            .invoke("add-slide", &json!({"layout": "sideways"}))
            .is_err(),
        "a layout outside the choices is refused"
    );
}
