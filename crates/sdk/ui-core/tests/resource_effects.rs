//! The typed effect host a session surface publishes and answers through.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_ui::automerge::{AutoCommit, ROOT};
use tilo_ui::tilo_actions::{
    ActionOutput, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, Resource,
};
use tilo_ui::widget::resource_effects::{EffectVocabulary, TypedEffects, host};
use tilo_ui::widget::{Button, Widget};

const PROBE: &str = "test.probe";

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum ProbeEffect {
    Increment,
    SetValue { value: i64 },
}

impl EffectVocabulary for ProbeEffect {
    const OPERATION_TYPES: &'static [&'static str] = &[PROBE];

    fn decode(type_id: &str, payload: &[u8]) -> Result<Self, String> {
        if type_id != PROBE {
            return Err(format!("not a probe operation: {type_id}"));
        }
        serde_json::from_slice(payload).map_err(|_| "malformed probe operation".to_owned())
    }
}

struct Handles {
    value: Rc<Cell<i64>>,
    answered: Rc<Cell<i64>>,
    viewed: Rc<Cell<bool>>,
}

struct Probe {
    button: Button,
    value: Rc<Cell<i64>>,
    answered: Rc<Cell<i64>>,
}

impl Probe {
    fn new() -> (Self, Handles) {
        let value = Rc::new(Cell::new(0));
        let answered = Rc::new(Cell::new(0));
        let viewed = Rc::new(Cell::new(false));
        let seen = Rc::clone(&viewed);
        (
            Self {
                button: Button::new("probe-child", "Child").on_view(move || seen.set(true)),
                value: Rc::clone(&value),
                answered: Rc::clone(&answered),
            },
            Handles {
                value,
                answered,
                viewed,
            },
        )
    }

    fn resource() -> Resource {
        let increment = ActionSpec::typed_operation(
            "increment",
            "Increment",
            "",
            OperationKind::Effect,
            PROBE,
            Vec::new(),
            |_| EncodedAction::encode_operation(PROBE, &ProbeEffect::Increment).unwrap(),
        );
        let set = ActionSpec::typed_operation(
            "set-value",
            "Set value",
            "",
            OperationKind::Effect,
            PROBE,
            vec![ActionParam::required(
                "value",
                ParamType::Integer,
                "New value",
            )],
            |args| {
                EncodedAction::encode_operation(
                    PROBE,
                    &ProbeEffect::SetValue {
                        value: args.integer("value").unwrap_or_default(),
                    },
                )
                .unwrap()
            },
        );
        Resource::new("test.probe:root", PROBE, "Probe")
            .with_action(increment)
            .with_action(set)
    }
}

impl Widget for Probe {
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.button]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.button]
    }

    fn build_layout(
        &mut self,
        taffy: &mut tilo_ui::taffy::TaffyTree<()>,
        theme: &tilo_ui::Theme,
        node_map: &mut HashMap<tilo_ui::taffy::NodeId, *mut dyn Widget>,
    ) -> tilo_ui::taffy::NodeId {
        self.button.build_layout(taffy, theme, node_map)
    }

    fn name(&self) -> &'static str {
        "Probe"
    }
}

impl TypedEffects for Probe {
    type Effect = ProbeEffect;

    fn published_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        vec![Self::resource()]
    }

    fn answer(&mut self, effect: ProbeEffect) -> Result<ActionOutput, String> {
        match effect {
            ProbeEffect::Increment => {
                self.value.set(self.value.get().saturating_add(1));
            }
            ProbeEffect::SetValue { value } => self.value.set(value),
        }
        self.answered.set(self.answered.get().saturating_add(1));
        Ok(ActionOutput::default())
    }
}

fn encoded(effect: &ProbeEffect) -> EncodedAction {
    EncodedAction::encode_operation(PROBE, effect).unwrap()
}

#[test]
fn the_host_publishes_the_roots_resources_and_declares_its_vocabulary() {
    let (probe, _handles) = Probe::new();
    let root = host(probe);
    let resources = tilo_ui::resources::resources(&root, &AutoCommit::new());
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].id.as_str(), "test.probe:root");
    let handlers = tilo_ui::widget::resource_operation_handlers(&root);
    assert!(handlers.contains(PROBE));
    assert!(
        handlers.contains("tilo.ui.control.invoke"),
        "the toolkit child's own control door stays declared"
    );
    let violations = tilo_ui::tilo_actions::validate_surface(
        &resources,
        &tilo_ui::tilo_actions::ActionRegistry::new(),
        &tilo_ui::widget::resource_operation_handlers(&root),
    );
    assert!(violations.is_empty(), "{violations:?}");
}

#[test]
fn the_app_host_accepts_the_published_surface() {
    let (probe, _handles) = Probe::new();
    let mut app = tilo_ui::App::new(tilo_ui::State::new(), Box::new(host(probe)));
    app.init_layout();
    let resources = app.collect_resources();
    assert!(
        resources
            .iter()
            .any(|resource| resource.id.as_str() == "test.probe:root"),
        "the app host withdrew the typed effect surface ({} resources)",
        resources.len()
    );
}

#[test]
fn a_typed_effect_reaches_the_roots_answer_and_a_malformed_one_is_refused() {
    let (probe, handles) = Probe::new();
    let mut root = host(probe);
    let start = |root: &mut _, encoded: &EncodedAction| {
        let mut doc = AutoCommit::new();
        tilo_ui::host_operation::start_operation(
            root,
            &tilo_ui::host_operation::ResourceOperation::local(
                tilo_ui::tilo_actions::InvocationId::new(),
                OperationKind::Effect,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut tilo_ui::widget::Edits::lend(&mut doc),
            &ROOT,
        )
    };

    let started = start(&mut root, &encoded(&ProbeEffect::Increment)).unwrap();
    assert!(matches!(
        started,
        Some(tilo_ui::host_operation::ResourceOperationStart::Completed(
            _
        ))
    ));
    assert_eq!(handles.value.get(), 1);
    assert_eq!(handles.answered.get(), 1);

    start(&mut root, &encoded(&ProbeEffect::SetValue { value: 7 })).unwrap();
    assert_eq!(handles.value.get(), 7);

    let malformed = EncodedAction {
        type_id: PROBE.into(),
        payload: br#"{"op":"explode"}"#.to_vec(),
    };
    let refused = start(&mut root, &malformed);
    assert_eq!(
        refused.as_ref().err().map(String::as_str),
        Some("malformed probe operation"),
        "{refused:?}"
    );
    assert_eq!(handles.answered.get(), 2);
}

#[test]
fn an_operation_outside_the_vocabulary_routes_to_the_toolkit_children() {
    let (probe, handles) = Probe::new();
    let mut root = host(probe);
    let mut doc = AutoCommit::new();
    let unknown = EncodedAction {
        type_id: "test.unknown".into(),
        payload: Vec::new(),
    };
    let started = tilo_ui::host_operation::start_operation(
        &mut root,
        &tilo_ui::host_operation::ResourceOperation::local(
            tilo_ui::tilo_actions::InvocationId::new(),
            OperationKind::Effect,
            &unknown.type_id,
            &unknown.payload,
        ),
        &mut tilo_ui::widget::Edits::lend(&mut doc),
        &ROOT,
    )
    .unwrap();
    assert!(started.is_none());
    assert_eq!(handles.answered.get(), 0);
    assert_eq!(handles.value.get(), 0);
}

#[test]
fn a_closure_bound_child_control_is_invokable_through_the_app_door() {
    let (probe, handles) = Probe::new();
    let mut app = tilo_ui::App::new(tilo_ui::State::new(), Box::new(host(probe)));
    app.init_layout();
    app.invoke_local_resource_action(
        &tilo_ui::tilo_actions::ActionAddress::new("tilo.control:probe-child", "invoke"),
        &serde_json::Map::new(),
    )
    .expect("the published control action");
    assert!(handles.viewed.get());
}
