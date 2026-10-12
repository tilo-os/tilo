#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! One published tree: the edits views and surfaces bind to where the user
//! is, and the live external rows a surface shows, join the resource's own
//! publication, and nothing but the resource's actions changes the
//! document.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilo_actions::{Action, ActionError, ActionParam, ActionSpec, OperationKind, ParamType};
use tilo_app::advanced::{NodeId, TaffyTree};
use tilo_app::automerge::transaction::Transactable;
use tilo_app::automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use tilo_app::framework::EffectAnswer;
use tilo_app::prelude::*;
use tilo_resource::{Doc, ModelError};

const ACTION_TYPE: &str = "test.tally.action";
const TOTAL: &str = "tally/total";

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
enum TallyAction {
    Add { by: i64 },
    Double,
}

impl Action for TallyAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        let next = match self {
            Self::Add { by } => count(doc).saturating_add(*by),
            Self::Double => count(doc).saturating_mul(2),
        };
        doc.put(ROOT, "count", next)?;
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

/// A count, with one child (its total) and slots resolved only by id.
struct Tally;

impl Resource for Tally {
    const KIND: &'static str = "test.tally";
    const LABEL: &'static str = "Tally";
    const ACTION_TYPE: &'static str = ACTION_TYPE;
    type State = i64;
    type Action = TallyAction;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(count(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new("double", "Double the count", Vec::new(), |_, _| {
                Ok(TallyAction::Double)
            })
            .describe("Add the count to itself."),
        ]
    }

    fn children(count: &i64) -> Vec<tilo_actions::Resource> {
        vec![
            tilo_actions::Resource::new(TOTAL, "test.tally.total", format!("Total {count}"))
                .with_action(JobEffect::Reveal { at: *count }.bound()),
        ]
    }

    fn child(_: &i64, id: &str) -> Option<tilo_actions::Resource> {
        let slot: i64 = id.strip_prefix("tally/slot:")?.parse().ok()?;
        let fill = ActionDecl::<Self>::new("fill", "Fill the slot", Vec::new(), move |_, _| {
            Ok(TallyAction::Add { by: slot })
        });
        Some(
            tilo_actions::Resource::new(id, "test.tally.slot", format!("Slot {slot}"))
                .with_action(tilo_resource::declaration_spec(&fill)),
        )
    }
}

/// How the surface publishes as a widget, beside its live rows.
#[derive(Clone, Copy, Default, PartialEq)]
enum Raw {
    #[default]
    Nothing,
    /// A hand-built document action the surface publishes itself.
    DocumentAction,
}

/// What the jobs surface does outside the document, each offered on the
/// row or child it acts on.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "test.tally.jobs")]
enum JobEffect {
    /// Stop the job.
    #[effect(row, returns)]
    Stop { job: u32 },
    /// Inspect the total.
    #[effect(view, row)]
    Inspect { job: u32 },
    /// Reveal the total.
    #[effect(view, row)]
    Reveal { at: i64 },
}

type Effects = Rc<RefCell<Vec<JobEffect>>>;

thread_local! {
    static RAW: std::cell::Cell<Raw> = const { std::cell::Cell::new(Raw::Nothing) };
    static EFFECTS: RefCell<Option<Effects>> = const { RefCell::new(None) };
}

struct TallyApp;

impl ResourceApp for TallyApp {
    type Resource = Tally;
    const ID: &'static str = "so.tilo.test-tally";
    const TITLE: &'static str = "Tally";

    fn view(cx: &mut ViewCx<Tally>) -> Element {
        // The step is view state: it lives here, not in the document.
        let step = LocalState::new(5_i64);
        let bound = step.clone();
        cx.bind_declarations(move |_| {
            let by = bound.get();
            vec![ActionDecl::new(
                "add-step",
                "Add the step",
                Vec::new(),
                move |_, _| Ok(TallyAction::Add { by }),
            )]
        });
        Box::new(
            Column::new()
                .push(Box::new(
                    Button::new("step-up", "Step up")
                        .on_click(move || step.set(step.get().saturating_add(1))),
                ))
                .push(cx.surface(Jobs::default())),
        )
    }
}

/// A surface showing jobs that run outside the document, with one selected.
#[derive(Default)]
struct Jobs {
    rect: Rect,
    selected: i64,
    effects: Effects,
}

fn job(id: u32) -> LiveRow {
    LiveRow::new(format!("job:{id}"), "test.job", format!("Job {id}"))
        .effect(&JobEffect::Stop { job: id })
}

impl Widget for Jobs {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn name(&self) -> &'static str {
        "Jobs"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    tilo_app::provides!(dyn tilo_app::resources::PublishesResources);
}

impl tilo_app::resources::PublishesResources for Jobs {
    fn resources(&self, _: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if RAW.get() == Raw::Nothing {
            return Vec::new();
        }
        vec![
            tilo_actions::Resource::new("rogue", "test.rogue", "Rogue").with_action(
                ActionSpec::immediate("bump", "Bump", ACTION_TYPE, &TallyAction::Add { by: 1 })
                    .unwrap(),
            ),
        ]
    }
}

impl ResourceSurface<Tally, JobEffect> for Jobs {
    fn present(&mut self, count: &Rc<i64>) {
        self.selected = **count;
    }

    fn attach(&mut self, _: SurfaceCx<Tally>) {
        self.selected = 3;
        EFFECTS.with(|effects| *effects.borrow_mut() = Some(self.effects.clone()));
    }

    fn effect(&mut self, effect: JobEffect, _: &mut EffectCx<Tally, Self>) -> EffectAnswer {
        self.effects.borrow_mut().push(effect.clone());
        Ok(match effect {
            JobEffect::Stop { job } => Effected::value(json!({ "stopped": job })),
            JobEffect::Inspect { .. } | JobEffect::Reveal { .. } => Effected::done(),
        })
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<Tally>> {
        let by = self.selected.max(1);
        vec![
            ActionDecl::new(
                "add-selected",
                "Add the selected amount",
                vec![ActionParam::optional(
                    "times",
                    ParamType::Integer,
                    "How many times",
                )],
                move |args, _| {
                    let times = args.integer("times").unwrap_or(1);
                    Ok(TallyAction::Add {
                        by: by.saturating_mul(times),
                    })
                },
            ),
            // The resource already publishes `double`; its own stays.
            ActionDecl::new("double", "Shadowed", Vec::new(), |_, _| {
                Ok(TallyAction::Add { by: 1000 })
            }),
        ]
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        vec![
            job(1),
            job(2),
            // Names the resource's own child: its operation joins it.
            LiveRow::new(TOTAL, "test.job.total", "Jobs total")
                .effect(&JobEffect::Inspect { job: 0 })
                .presentation(tilo_actions::ResourcePresentation {
                    detail: Some("2 jobs running".into()),
                    badge: None,
                    preview: None,
                }),
        ]
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        let id = id.strip_prefix("job:")?.parse().ok()?;
        Some(job(id))
    }
}

fn published(app: &mut TestApp<TallyApp>) -> Vec<tilo_actions::Resource> {
    app.harness().resources()
}

fn find<'a>(
    resources: &'a [tilo_actions::Resource],
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource)
        } else {
            find(&resource.children, id)
        }
    })
}

fn action_ids(resource: &tilo_actions::Resource) -> Vec<&str> {
    resource
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect()
}

#[test]
fn live_rows_and_bound_edits_join_the_resource_tree() {
    RAW.set(Raw::Nothing);
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let effects = EFFECTS.with(|effects| effects.borrow().clone()).unwrap();
    let resources = published(&mut app);

    // Live rows are children of the resource, carrying only their
    // operations; one naming a document child joins it, so no id is
    // published twice.
    let root = find(&resources, Tally::KIND).unwrap();
    let children: Vec<&str> = root
        .children
        .iter()
        .map(|child| child.id.as_str())
        .collect();
    assert_eq!(children, [TOTAL, "job:1", "job:2"]);
    let total = find(&resources, TOTAL).unwrap();
    assert_eq!(total.label, "Total 0", "the document child keeps its label");
    assert_eq!(action_ids(total), ["reveal", "inspect"]);
    assert!(
        total
            .actions
            .iter()
            .all(|action| action.kind == OperationKind::View),
        "presentation is offered as a view"
    );
    assert_eq!(
        total
            .presentation
            .as_ref()
            .and_then(|presentation| presentation.detail.as_deref()),
        Some("2 jobs running"),
        "what only the surface knows presents the child"
    );
    assert_eq!(
        find(&resources, "job:2").unwrap().actions[0].kind,
        OperationKind::Effect
    );
    for row_only in ["stop", "inspect", "reveal"] {
        assert!(
            !action_ids(root).contains(&row_only),
            "`{row_only}` is offered only on what it acts on"
        );
    }

    // A live row's effect reaches the surface's handler through the door as
    // the value the row was bound to, and answers its caller; it changes
    // nothing in the document. So does one a document child carries.
    let output = app.call_on("job:2", "stop", json!({})).unwrap();
    assert_eq!(output, Some(json!({ "stopped": 2 })));
    app.call_on(TOTAL, "reveal", json!({})).unwrap();
    app.call_on(TOTAL, "inspect", json!({})).unwrap();
    assert_eq!(
        *effects.borrow(),
        [
            JobEffect::Stop { job: 2 },
            JobEffect::Reveal { at: 0 },
            JobEffect::Inspect { job: 0 },
        ]
    );
    assert_eq!(app.state(), 0);

    // A row the tree leaves out resolves by id, at the surface; a child the
    // resource leaves out resolves at the resource.
    assert!(find(&published(&mut app), "job:7").is_none());
    let output = app.call_on("job:7", "stop", json!({})).unwrap();
    assert_eq!(output, Some(json!({ "stopped": 7 })));
    assert!(find(&published(&mut app), "tally/slot:4").is_none());
    app.call_on("tally/slot:4", "fill", json!({})).unwrap();
    assert_eq!(app.state(), 4);

    // Edits bound to where the user is are typed actions of the resource:
    // the surface's selection, the view's step; the resource's own wins.
    let root = find(&resources, Tally::KIND).unwrap().clone();
    for id in ["add-selected", "add-step", "double"] {
        let spec = root.actions.iter().find(|action| action.id == id).unwrap();
        assert_eq!(spec.kind, OperationKind::Document, "{id}");
    }
    app.call("add-selected", json!({ "times": 2 })).unwrap();
    assert_eq!(app.state(), 12, "the surface was presented 4: twice that");
    app.call("double", json!({})).unwrap();
    assert_eq!(app.state(), 24, "the resource's own `double` wins");
    app.harness().click_named("step-up").unwrap();
    app.call("add-step", json!({})).unwrap();
    assert_eq!(app.state(), 30, "the step as the view holds it now");

    // Each is an undoable step, like any declared action.
    assert!(app.undo());
    assert_eq!(app.state(), 24);
}

#[test]
#[cfg_attr(
    debug_assertions,
    should_panic(expected = "offers document action `bump` outside the resource's actions")
)]
fn a_surface_cannot_publish_a_document_action_of_its_own() {
    RAW.set(Raw::DocumentAction);
    let mut app = TestApp::<TallyApp>::new().unwrap();
    let resources = published(&mut app);
    let rogue = find(&resources, "rogue").expect("the row itself is published");
    assert!(rogue.actions.is_empty(), "its document action is dropped");
    assert!(app.call_on("rogue", "bump", json!({})).is_err());
    assert_eq!(app.state(), 0);
}

thread_local! {
    static RETAINED_ACTIONS: RefCell<Option<tilo_app::framework::Actions<Tally>>> = const { RefCell::new(None) };
}

struct DynamicBindingsApp;

impl ResourceApp for DynamicBindingsApp {
    type Resource = Tally;
    const ID: &'static str = "so.tilo.test-dynamic-bindings";
    const TITLE: &'static str = "Dynamic bindings";

    fn view(cx: &mut ViewCx<Tally>) -> Element {
        RETAINED_ACTIONS.with(|actions| *actions.borrow_mut() = Some(cx.actions()));
        cx.bind_declarations(|count| {
            let mut declarations = vec![ActionDecl::new("seed", "Seed", Vec::new(), |_, _| {
                Ok(TallyAction::Add { by: 1 })
            })];
            if *count > 0 {
                declarations.push(ActionDecl::new(
                    "conditional",
                    "Conditional",
                    Vec::new(),
                    |_, _| Ok(TallyAction::Add { by: 1 }),
                ));
            }
            declarations
        });
        Box::new(Text::new("Dynamic declarations"))
    }
}

#[test]
fn retained_actions_follow_canonical_publication_through_edits_and_undo() {
    use tilo_resource::ActionRef;
    let mut app = TestApp::<DynamicBindingsApp>::new().unwrap();
    let actions = RETAINED_ACTIONS
        .with(|actions| actions.borrow().clone())
        .unwrap();
    let conditional = ActionRef::<Tally, ()>::new("conditional", &[]);
    let root = actions
        .binding::<()>(ActionRef::<Tally, ()>::new("seed", &[]))
        .unwrap()
        .address()
        .resource
        .clone();
    assert!(actions.binding::<()>(conditional).is_none());
    app.invoke(ActionRef::<Tally, ()>::new("seed", &[]).invoke())
        .unwrap();
    let binding = actions
        .binding::<()>(conditional)
        .expect("new declaration is bindable");
    assert_eq!(binding.address().resource, root);
    assert!(app.undo());
    assert!(
        actions.binding::<()>(conditional).is_none(),
        "removed declaration cannot bind"
    );
    assert!(app.redo());
    assert_eq!(
        actions
            .binding::<()>(conditional)
            .unwrap()
            .address()
            .resource,
        root
    );
}
