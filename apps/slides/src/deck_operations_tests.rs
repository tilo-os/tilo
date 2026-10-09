//! The deck's operations, driven the way an agent drives them: one call
//! each, on the deck or on the slide it acts on, through the door the host
//! uses.

#![allow(
    clippy::float_cmp,
    reason = "every compared number is a literal the test itself passed in, stored unchanged"
)]

use serde_json::{Value, json};
use tilo_actions::DoorError;
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Edit, Invoke};

use crate::SlidesShell;
use crate::actions;
use crate::deck::{Deck, mirror};
use crate::model::{ElementKind, TextAlign, seed_deck};

fn seeded() -> TestApp<SlidesShell> {
    let mut app = TestApp::<SlidesShell>::new().unwrap();
    app.invoke(Invoke::apply(&actions::replace_deck_action(&seed_deck())).unwrap())
        .unwrap();
    app
}

/// The deck as the editor draws it.
fn deck(app: &TestApp<SlidesShell>) -> crate::model::Deck {
    mirror(&app.state())
}

fn on_slide(
    app: &mut TestApp<SlidesShell>,
    slide: &str,
    action: &str,
    args: Value,
) -> Result<Option<Value>, DoorError> {
    app.call_on(&actions::slide_resource_id(slide), action, args)
}

fn field_edit(app: &mut TestApp<SlidesShell>, edit: &Edit<Deck>) -> Result<(), DoorError> {
    app.call("field-edit", json!({ "edit": edit })).map(drop)
}

#[test]
fn the_deck_publishes_its_verbs_on_the_deck_and_on_each_slide() {
    let mut app = seeded();
    let surface = app.agent_surface();
    let rows = surface.as_array().unwrap();
    let row = |resource: &str, action: &str| {
        rows.iter()
            .find(|row| row["resource"] == resource && row["action"] == action)
            .cloned()
    };
    for action in [
        "add-slide",
        "insert-slide",
        "replace-deck",
        ui::tilo_resource::APPLY,
        ui::tilo_resource::GROUP,
        ui::tilo_resource::FIELD_EDIT,
    ] {
        assert!(
            row("slides.deck", action).is_some(),
            "`{action}` is published"
        );
    }
    let slides = deck(&app).slides;
    for slide in &slides {
        let item = actions::slide_resource_id(&slide.id);
        for element in &slide.elements {
            let item = actions::element_resource_id(&slide.id, &element.id);
            for action in ["delete-element", "bring-to-front", "send-to-back"] {
                assert!(row(&item, action).is_some(), "`{action}` is on {item}");
            }
        }
        for action in [
            "add-element",
            "move-slide",
            "duplicate-slide",
            "delete-slide",
            "set-text",
        ] {
            assert!(row(&item, action).is_some(), "`{action}` is on {item}");
        }
    }
    let add = row(&actions::slide_resource_id(&slides[0].id), "add-element").unwrap();
    let params = add["params"].to_string();
    for name in [
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
    ] {
        assert!(
            params.contains(&format!("\"{name}\"")),
            "add-element takes `{name}`: {params}"
        );
    }
}

#[test]
fn add_element_creates_it_on_top_with_a_minted_id() {
    let mut app = seeded();
    let before = deck(&app);
    let slide = &before.slides[4];
    on_slide(
        &mut app,
        &slide.id,
        "add-element",
        json!({"element": {
            "kind": "text",
            "x": 110, "y": 540, "w": 900, "h": 60,
            "text": "hello@example.com",
            "text_color": "text-dim",
            "font_size": 28,
            "align": "center",
            "italic": true,
        }}),
    )
    .expect("add-element applies");
    let after = deck(&app);
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
    assert_eq!(
        after.slides[4].z_sorted().last().map(|e| &e.id),
        Some(&added.id),
        "the new element is above everything"
    );
    assert!(
        uuid::Uuid::parse_str(&added.id).is_ok(),
        "the id comes from the action's context"
    );
}

#[test]
fn add_element_refuses_what_the_inspector_could_not_make() {
    let mut app = seeded();
    let slide = deck(&app).slides[0].id.clone();
    let versions = app.versions().len();
    for element in [
        json!({"kind": "star", "x": 0, "y": 0, "w": 10, "h": 10}),
        json!({"kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10, "fill": "plaid"}),
        json!({"kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10, "text": "x"}),
        json!({"kind": "image", "x": 0, "y": 0, "w": 10, "h": 10}),
        json!({"kind": "text", "x": 0, "y": 0, "w": 10, "h": 10,
               "fill": "accent", "text_color": "white"}),
        json!({"kind": "rect", "x": 0, "y": 0, "w": 0, "h": 10}),
    ] {
        assert!(
            on_slide(
                &mut app,
                &slide,
                "add-element",
                json!({ "element": element })
            )
            .is_err(),
            "refused: {element}"
        );
        assert_eq!(app.versions().len(), versions, "nothing written: {element}");
    }
    assert!(
        app.call_on(
            &actions::slide_resource_id("no-such-slide"),
            "add-element",
            json!({"element": {"kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10}}),
        )
        .is_err(),
        "a slide that is not there is refused"
    );
}

#[test]
fn an_element_is_moved_and_resized_by_field_edits() {
    let mut app = seeded();
    let slide = deck(&app).slides[2].clone();
    let target = slide.elements[3].clone();
    let paths = actions::element_paths(&slide.id, &target.id);
    field_edit(&mut app, &paths.x.set(140.0)).unwrap();
    field_edit(&mut app, &paths.y.set(260.0)).unwrap();
    let moved = deck(&app).slides[2].element(&target.id).cloned().unwrap();
    assert_eq!(
        (moved.x, moved.y, moved.w, moved.h),
        (140.0, 260.0, target.w, target.h)
    );

    for edit in actions::resize_element_actions(&slide.id, &target.id, (150.0, 270.0, 400.0, 40.0))
    {
        app.invoke(Invoke::apply(&edit).unwrap()).unwrap();
    }
    let resized = deck(&app).slides[2].element(&target.id).cloned().unwrap();
    assert_eq!(
        (resized.x, resized.y, resized.w, resized.h),
        (150.0, 270.0, 400.0, 40.0)
    );

    assert!(
        field_edit(&mut app, &paths.w.set(0.0)).is_err(),
        "a width the slide cannot show is refused"
    );
}

#[test]
fn restyling_changes_only_the_named_styles() {
    let mut app = seeded();
    // The closing slide's subtitle: the text that must stay legible.
    let slide = deck(&app).slides[4].clone();
    let target = slide.elements[2].clone();
    let paths = actions::element_paths(&slide.id, &target.id);
    field_edit(
        &mut app,
        &paths.fill.set(ui::tilo_resource::Atom("white".into())),
    )
    .unwrap();
    field_edit(&mut app, &paths.font_size.set(36.0)).unwrap();
    field_edit(&mut app, &paths.align.set(TextAlign::Right)).unwrap();
    let bold = actions::restyle_action(&slide.id, &target, &actions::Style::Bold(true));
    app.invoke(Invoke::apply(&bold).unwrap()).unwrap();
    let styled = deck(&app).slides[4].element(&target.id).cloned().unwrap();
    assert_eq!(styled.fill, "white");
    assert!(styled.bold, "every word of the box is bold");
    assert_eq!(styled.font_size, 36.0);
    assert_eq!(styled.align, TextAlign::Right);
    assert_eq!(styled.italic, target.italic);
    assert_eq!(styled.stroke, target.stroke);
    assert_eq!(styled.text, target.text);
    assert_eq!((styled.x, styled.y), (target.x, target.y));

    assert!(
        field_edit(
            &mut app,
            &paths.fill.set(ui::tilo_resource::Atom("plaid".into()))
        )
        .is_err(),
        "a colour outside the palette is refused"
    );
}

#[test]
fn delete_element_is_destructive_and_needs_confirmation() {
    let mut app = seeded();
    let slide = deck(&app).slides[1].clone();
    let target = slide.elements[0].id.clone();
    let args = json!({});
    let item = actions::element_resource_id(&slide.id, &target);
    assert!(matches!(
        app.call_on(&item, "delete-element", args.clone()),
        Err(DoorError::ConfirmationRequired)
    ));
    assert!(deck(&app).slides[1].element(&target).is_some());
    app.call_confirmed_on(&item, "delete-element", args)
        .expect("a confirmed delete applies");
    assert!(deck(&app).slides[1].element(&target).is_none());
}

#[test]
fn move_slide_reorders_the_deck() {
    let mut app = seeded();
    let ids = |app: &TestApp<SlidesShell>| -> Vec<String> {
        deck(app).slides.iter().map(|s| s.id.clone()).collect()
    };
    let order = ids(&app);
    on_slide(&mut app, &order[4], "move-slide", json!({"position": 1}))
        .expect("move-slide applies");
    assert_eq!(
        ids(&app),
        [&order[0], &order[4], &order[1], &order[2], &order[3]].map(String::clone)
    );
    on_slide(&mut app, &order[4], "move-slide", json!({"position": 4}))
        .expect("and back to the end");
    assert_eq!(ids(&app), order);
    assert!(
        on_slide(&mut app, &order[0], "move-slide", json!({"position": 9})).is_err(),
        "a position past the end is refused"
    );
}

#[test]
fn a_group_applies_several_operations_as_one_change() {
    let mut app = seeded();
    let before = deck(&app);
    let slide = ui::tilo_resource::Id::from(before.slides[3].id.as_str());
    let add = || {
        Deck::ADD_ELEMENT.with(crate::deck::deck::AddElement {
            slide: slide.clone(),
            element: serde_json::from_value(
                json!({"kind": "rect", "x": 96, "y": 600, "w": 300, "h": 40}),
            )
            .unwrap(),
        })
    };
    let versions = app.versions().len();
    app.invoke(Invoke::group([
        add(),
        add(),
        Deck::MOVE_SLIDE.with(crate::deck::deck::MoveSlide {
            slide: slide.clone(),
            position: 0,
        }),
    ]))
    .expect("the group applies");
    assert_eq!(app.versions().len(), versions + 1, "one change");
    let after = deck(&app);
    assert_eq!(after.slides[0].id, slide.as_str());
    let added: Vec<_> = after.slides[0]
        .elements
        .iter()
        .filter(|e| before.slides[3].element(&e.id).is_none())
        .collect();
    assert_eq!(added.len(), 2, "two identical requests add two elements");
    assert_ne!(added[0].id, added[1].id, "each member mints its own id");
}

#[test]
fn apply_takes_any_deck_action_as_json() {
    let mut app = seeded();
    let slide = deck(&app).slides[1].id.clone();
    let action = actions::set_slide_background_action(&slide, crate::model::Background::Deep);
    app.call("apply", json!({ "action": action }))
        .expect("apply applies");
    assert_eq!(
        deck(&app).slides[1].background,
        crate::model::Background::Deep
    );
    // A destructive value asks for confirmation through `apply` as well.
    let element = deck(&app).slides[1].elements[0].id.clone();
    let delete = actions::delete_element_action(&slide, &element);
    assert!(matches!(
        app.call("apply", json!({ "action": delete })),
        Err(DoorError::ConfirmationRequired)
    ));
}

#[test]
fn add_slide_writes_its_title_and_body_in_one_step() {
    let mut app = seeded();
    let before = deck(&app).slides.len();
    app.call(
        "add-slide",
        json!({"layout": "title", "title": "Product Launch", "body": "October 2026"}),
    )
    .unwrap();
    app.call(
        "add-slide",
        json!({"title": "Agenda", "body": "Goals\nTimeline\nBudget"}),
    )
    .unwrap();
    app.call("add-slide", json!({})).unwrap();
    let deck = deck(&app);
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
        app.call("add-slide", json!({"layout": "sideways"}))
            .is_err(),
        "a layout outside the choices is refused"
    );
}

/// An agent's changes read back as the app's own sentences.
#[test]
fn an_agents_changes_read_as_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    ui::i18n::I18nManager::init("en-US");
    let mut app = seeded();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Deck as ui::tilo_resource::Resource>::KIND;
    let call = |app: &mut TestApp<SlidesShell>, resource: &str, id: &str, args: Value| {
        app.call_by(agent.clone(), resource, id, args)
            .unwrap_or_else(|error| panic!("{id}: {error:?}"));
    };
    call(&mut app, kind, "add-slide", json!({ "title": "Budget" }));
    let slide = deck(&app).slides.last().unwrap().id.clone();
    let item = actions::slide_resource_id(&slide);
    call(
        &mut app,
        &item,
        "set-text",
        json!({ "body": "Costs\nIncome" }),
    );
    let title = deck(&app).slides.last().unwrap().elements[0].id.clone();
    let paths = actions::element_paths(&slide, &title);
    call(
        &mut app,
        kind,
        "field-edit",
        json!({ "edit": paths.x.set(120.0) }),
    );
    call(
        &mut app,
        kind,
        "field-edit",
        json!({ "edit": paths.bold.set(true) }),
    );
    call(
        &mut app,
        &actions::element_resource_id(&slide, &title),
        "bring-to-front",
        json!({}),
    );
    call(
        &mut app,
        kind,
        "field-edit",
        json!({ "edit": Deck::paths().title.replace("Plan") }),
    );

    let english = include_str!("../locales/en-US.ftl");
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        [
            "Added a slide",
            "Rewrote a slide's text",
            "Moved an element",
            "Made an element bold",
            "Brought an element to the front",
            "Renamed the deck",
        ]
    );
    // An element's edits and its item verbs are about the element, named by
    // its rich-text body's first line, not by its id or its slide's.
    let named: Vec<(String, String)> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .skip(2)
        .take(3)
        .map(|entry| {
            let item = entry.item.expect("an element's change names the element");
            (item.id, item.label)
        })
        .collect();
    assert_eq!(named, vec![(title.clone(), "Budget".to_owned()); 3]);
}

/// The deck's verbs answer their caller with the outline of the deck they
/// left, so an agent sees what it changed without reading the deck again.
#[test]
fn deck_edits_answer_with_the_outline() {
    let mut app = seeded();
    for (resource, action, args) in [
        (
            "slides.deck".to_owned(),
            "add-slide",
            json!({ "title": "Product Launch" }),
        ),
        (
            actions::slide_resource_id(&deck(&app).slides[0].id),
            "set-text",
            json!({ "title": "Opening" }),
        ),
        (
            actions::slide_resource_id(&deck(&app).slides[1].id),
            "move-slide",
            json!({ "position": 0 }),
        ),
    ] {
        let answer = app.call_on(&resource, action, args).unwrap();
        let outline = crate::outline::of_deck(&deck(&app));
        assert_eq!(
            answer
                .as_ref()
                .and_then(|answer| answer["outline"].as_str()),
            Some(outline.as_str()),
            "`{action}` answers with the outline"
        );
    }
}
