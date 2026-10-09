//! The published resource tree is the app's public surface: building it
//! faster must not change one byte of it.
//!
//! `publication.golden.json` was recorded from the build that walked the
//! whole view once per row. The app below exercises everything that build
//! decorated: document children, live rows (one joining a child, one of its
//! own), edits two surfaces bind to rows (one offered by both, where the
//! first surface's stays), edits bound to the view and to the viewer, and
//! the framework's own operations.
#![cfg(all(test, not(target_arch = "wasm32")))]

use std::cell::Cell;
use std::rc::Rc;

use serde_json::json;
use tilo_actions::{Action, ActionError, ActionParam, ParamType};
use tilo_resource::{Doc, ModelError};
use ui_core::automerge::transaction::Transactable;
use ui_core::automerge::{ROOT, ReadDoc, ScalarValue, Value};

use super::*;
use crate::advanced::{NodeId, TaffyTree};
use crate::prelude::*;

const ACTION_TYPE: &str = "test.shelf.action";

#[derive(
    Clone,
    Debug,
    tilo_resource::__private::serde::Serialize,
    tilo_resource::__private::serde::Deserialize,
)]
#[serde(
    crate = "tilo_resource::__private::serde",
    tag = "action",
    rename_all = "kebab-case"
)]
enum ShelfAction {
    Add { by: i64 },
}

impl Action for ShelfAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        let Self::Add { by } = self;
        doc.put(ROOT, "count", count(doc).saturating_add(*by))?;
        Ok(())
    }
}

fn count(doc: &AutoCommit) -> i64 {
    match doc.get(ROOT, "count") {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Int(count) => *count,
            _ => 0,
        },
        _ => 0,
    }
}

fn add(id: &'static str, label: &str, by: i64) -> ActionDecl<Shelf> {
    ActionDecl::new(id, label, Vec::new(), move |_, _| {
        Ok(ShelfAction::Add { by })
    })
}

/// A shelf of `count` items, each a document child with an action of its own.
struct Shelf;

impl Resource for Shelf {
    const KIND: &'static str = "test.shelf";
    const LABEL: &'static str = "Shelf";
    const ACTION_TYPE: &'static str = ACTION_TYPE;
    type State = i64;
    type Action = ShelfAction;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(count(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "grow",
                "Grow the shelf",
                vec![ActionParam::required("by", ParamType::Integer, "How many")],
                |args, _| {
                    Ok(ShelfAction::Add {
                        by: args.integer("by").unwrap_or(1),
                    })
                },
            )
            .describe("Add items to the shelf."),
            add("double", "Double the shelf", 2),
        ]
    }

    fn children(count: &i64) -> Vec<tilo_actions::Resource> {
        (0..*count)
            .map(|item| {
                tilo_actions::Resource::new(
                    format!("shelf/item:{item}"),
                    "test.shelf.item",
                    format!("Item {item}"),
                )
                .with_action(tilo_resource::declaration_spec(&add(
                    "take",
                    "Take the item",
                    -1,
                )))
            })
            .collect()
    }
}

/// Which side of the shelf a surface stands on.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
}

/// A surface offering edits on rows. The left one also binds an edit to
/// the viewer and shows live rows.
struct Rows {
    side: Side,
    rect: Rect,
}

impl Widget for Rows {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy
            .new_leaf(ui_core::taffy::Style::default())
            .unwrap_or_else(|_| NodeId::from(0_u64))
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
    }

    fn name(&self) -> &'static str {
        "Rows"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Whether `row` names an even item.
fn even(row: &str) -> bool {
    row.strip_prefix("shelf/item:")
        .and_then(|item| item.parse::<u32>().ok())
        .is_some_and(|item| item % 2 == 0)
}

impl ResourceSurface<Shelf> for Rows {
    fn present(&mut self, _: &Rc<i64>) {}

    fn bound_declarations(&self) -> Vec<ActionDecl<Shelf>> {
        if self.side == Side::Right {
            return Vec::new();
        }
        let viewer = ui_core::invocation_scope::current_viewer();
        let viewer = viewer
            .as_ref()
            .map_or("nobody", tilo_actions::ParticipantId::as_str);
        vec![add("wave", &format!("Wave at {viewer}"), 1)]
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        if self.side == Side::Right {
            return Vec::new();
        }
        vec![
            LiveRow::new("shelf/item:1", "test.shelf.live", "Live item").presentation(
                tilo_actions::ResourcePresentation {
                    detail: Some("being looked at".into()),
                    badge: None,
                    preview: None,
                },
            ),
            // A row nesting one of its own: the nested row is bound too.
            LiveRow::new("shelf/visitor", "test.shelf.visitor", "Visitor").row(LiveRow::new(
                "shelf/visitor/guest",
                "test.shelf.guest",
                "Guest",
            )),
        ]
    }

    fn row_declarations(&self, row: &str) -> Vec<ActionDecl<Shelf>> {
        match self.side {
            Side::Left if even(row) => vec![
                add("label", "Label from the left", 2),
                add("mark-left", "Mark left", 3),
            ],
            Side::Left => vec![add("mark-left", "Mark left", 3)],
            Side::Right => vec![
                add("mark-right", "Mark right", 4),
                add("label", "Label from the right", 5),
            ],
        }
    }
}

thread_local! {
    /// How often the probe was walked through.
    static WALKS: Cell<usize> = const { Cell::new(0) };
}

/// A leaf that counts the walks of the view reaching it.
struct Probe(ui_core::widget::Text);

impl ui_core::widget::Decorator for Probe {
    type Inner = ui_core::widget::Text;
    fn inner(&self) -> &Self::Inner {
        &self.0
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.0
    }
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        WALKS.set(WALKS.get().saturating_add(1));
        self.0.for_each_child(visitor);
    }
}

struct ShelfApp;

impl ResourceApp for ShelfApp {
    type Resource = Shelf;
    const ID: &'static str = "so.tilo.test-shelf";
    const TITLE: &'static str = "Shelf";

    fn view(cx: &mut ViewCx<Shelf>) -> Element {
        cx.bind_declarations(|count| vec![add("step", "Step", count.saturating_add(1))]);
        Box::new(
            Column::new()
                .push(cx.surface(Rows {
                    side: Side::Left,
                    rect: Rect::ZERO,
                }))
                .push(Box::new(Probe(ui_core::widget::Text::new("probe"))))
                .push(cx.surface(Rows {
                    side: Side::Right,
                    rect: Rect::ZERO,
                })),
        )
    }
}

/// `resource` as inert data, children included: what agents, the palette
/// and accessibility read.
fn outline(resource: &tilo_actions::Resource) -> serde_json::Value {
    json!({
        "id": resource.id.as_str(),
        "kind": resource.kind,
        "label": resource.label,
        "detail": resource.presentation.as_ref().and_then(|presentation| presentation.detail.clone()),
        "actions": tilo_actions::door::survey(&[without_children(resource)]),
        "views": resource.views.len(),
        "children": resource.children.iter().map(outline).collect::<Vec<_>>(),
    })
}

/// The shelf app holding `items` items.
fn shelf(items: i64) -> Result<TestApp<ShelfApp>, String> {
    let mut app = TestApp::<ShelfApp>::new().map_err(|error| format!("{error:?}"))?;
    app.call("grow", json!({ "by": items }))
        .map_err(|error| format!("{error:?}"))?;
    Ok(app)
}

#[test]
fn the_published_tree_is_the_one_recorded_before_rows_were_indexed() {
    let mut app = shelf(3).expect("the shelf opens");
    let (nobody, ada) = app
        .harness()
        .with_widget_mut(|root: &mut ResourceRoot<Shelf>| {
            let nobody = outline(&root.publication());
            let _viewer = ui_core::invocation_scope::ViewerScope::enter(
                tilo_actions::ParticipantId::new("ada").expect("a participant id"),
            );
            (nobody, outline(&root.publication()))
        })
        .expect("the root is the shelf's");
    let recorded: serde_json::Value =
        serde_json::from_str(include_str!("publication.golden.json")).expect("valid JSON");
    assert_eq!(nobody, recorded);
    // What a surface binds to where someone is, it binds to that viewer.
    let waves = |outline: &serde_json::Value| {
        outline["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|action| action["address"]["action"] == "wave")
            .map(|action| action["action_label"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(waves(&nobody), [json!("Wave at nobody")]);
    assert_eq!(waves(&ada), [json!("Wave at ada")]);
    let at_ada = serde_json::to_string(&nobody)
        .unwrap_or_default()
        .replace("Wave at nobody", "Wave at ada");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&at_ada).unwrap_or_default(),
        ada,
        "the viewer changes nothing else"
    );
}

#[test]
fn rows_bind_the_edits_surfaces_offer_in_surface_order() {
    let mut app = shelf(3).expect("the shelf opens");
    let resources = app.harness().resources();
    let root = resources
        .iter()
        .find(|resource| resource.kind == Shelf::KIND)
        .expect("the shelf is published");
    let rows: Vec<(&str, Vec<(&str, &str)>)> = root
        .children
        .iter()
        .map(|child| {
            (
                child.id.as_str(),
                child
                    .actions
                    .iter()
                    .map(|action| (action.id.as_str(), action.label.as_str()))
                    .collect(),
            )
        })
        .collect();
    let even = vec![
        ("take", "Take the item"),
        ("label", "Label from the left"),
        ("mark-left", "Mark left"),
        ("mark-right", "Mark right"),
    ];
    let odd = vec![
        ("take", "Take the item"),
        ("mark-left", "Mark left"),
        ("mark-right", "Mark right"),
        ("label", "Label from the right"),
    ];
    let visitor = vec![
        ("mark-left", "Mark left"),
        ("mark-right", "Mark right"),
        ("label", "Label from the right"),
    ];
    let guest = root
        .children
        .iter()
        .find(|child| child.id.as_str() == "shelf/visitor")
        .and_then(|visitor| visitor.children.first())
        .map(|guest| {
            guest
                .actions
                .iter()
                .map(|action| (action.id.as_str(), action.label.as_str()))
                .collect::<Vec<_>>()
        });
    assert_eq!(
        guest,
        Some(visitor.clone()),
        "a nested row is bound as its parent is"
    );
    assert_eq!(
        rows,
        vec![
            ("shelf/item:0", even.clone()),
            ("shelf/item:1", odd.clone()),
            ("shelf/item:2", even),
            ("shelf/visitor", visitor),
        ]
    );
}

#[test]
fn one_publication_walks_the_view_twice_whatever_the_rows() {
    for items in [2, 40] {
        let mut app = shelf(items).expect("the shelf opens");
        let walks = app
            .harness()
            .with_widget_mut(|root: &mut ResourceRoot<Shelf>| {
                WALKS.set(0);
                let published = root.publication();
                assert!(published.children.len() > usize::try_from(items).unwrap_or(0));
                WALKS.get()
            })
            .expect("the root is the shelf's");
        assert_eq!(
            walks, 2,
            "{items} rows: one walk for what surfaces add, one for the edits they bind to rows"
        );
    }
}

#[test]
fn controls_bind_to_the_published_actions_without_its_children() {
    let mut app = shelf(5).expect("the shelf opens");
    app.harness()
        .with_widget_mut(|root: &mut ResourceRoot<Shelf>| {
            let published = root.publication();
            let bound = root.binding_publication(&root.binding_head());
            assert!(bound.children.is_empty(), "no child is built for bindings");
            let head = without_children(&published);
            assert_eq!(
                tilo_actions::door::survey(&[bound]),
                tilo_actions::door::survey(&[head]),
                "bindings see exactly the published resource's actions"
            );
            WALKS.set(0);
            let head = root.binding_head();
            root.binding_publication(&head);
            assert_eq!(WALKS.get(), 1, "bindings walk the view once");
        })
        .expect("the root is the shelf's");
}
