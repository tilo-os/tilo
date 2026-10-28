//! An adapter declares a legacy app's actions, in layers, so a caller with no
//! window reaches them exactly as it reaches an app's own.

use tilo_actions::adapter::{
    ADAPTER_OPERATION_TYPE, ActionAdapter, AdapterAction, AdapterInvocation, Check, Implementation,
    Layer, Observation, Skipped, Step, Undo,
};
use tilo_actions::contract::ActionKind;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::{ActionContract, IRREVERSIBLE_TAG};

fn call_step() -> Step {
    Step::Call {
        bus: "org.example.Legacy".into(),
        path: "/org/example/Legacy".into(),
        interface: "org.example.Legacy.Documents".into(),
        method: "Export".into(),
        arguments: Vec::new(),
    }
}

fn check() -> Check {
    Check::File {
        path: "/home/person/report.pdf".into(),
        bytes: 12_480,
    }
}

fn implementation(layer: Layer, steps: Vec<Step>, checks: Vec<Check>) -> Implementation {
    Implementation {
        layer,
        steps,
        checks,
        app_version: "24.*".into(),
        recorded_from: None,
    }
}

fn interface_layer() -> Implementation {
    implementation(Layer::Interface, vec![call_step()], vec![check()])
}

fn pixels_layer() -> Implementation {
    implementation(
        Layer::Pixels,
        vec![
            Step::Click { x: 812, y: 64 },
            Step::Type {
                text: "report.pdf".into(),
            },
            Step::Press {
                keys: vec!["enter".into()],
            },
        ],
        vec![check()],
    )
}

fn export_action(implementations: Vec<Implementation>) -> AdapterAction {
    AdapterAction {
        id: "export-pdf".into(),
        label: "Export as PDF".into(),
        keywords: Vec::new(),
        params: Vec::new(),
        contract: ActionContract::default(),
        undo: Undo::Restore,
        implementations,
    }
}

fn adapter(actions: Vec<AdapterAction>) -> ActionAdapter {
    ActionAdapter {
        app: "org.example.Legacy".into(),
        app_version: "24.*".into(),
        platform: "linux".into(),
        description: String::new(),
        actions,
    }
}

/// The layers are ordered most precise first, and that order — not the order
/// an author wrote them in — is the order an invocation falls back through.
#[test]
fn layers_fall_back_in_order_of_precision() {
    let action = export_action(vec![pixels_layer(), interface_layer()]);
    let ranked: Vec<Layer> = action
        .ranked()
        .iter()
        .map(|implementation| implementation.layer)
        .collect();
    assert_eq!(ranked, vec![Layer::Interface, Layer::Pixels]);

    assert!(Layer::Interface < Layer::Files);
    assert!(Layer::Files < Layer::Accessibility);
    assert!(Layer::Accessibility < Layer::Pixels);
    assert!(Layer::Pixels < Layer::Invalid);
    assert!(Layer::Native < Layer::Invalid);
    assert_eq!(Layer::Pixels.rank(), 4);
}

/// A layer is chosen only if it can be reached here, and every layer passed
/// over says why, so "none of them worked" is never the whole answer.
#[test]
fn a_plan_names_the_first_reachable_layer_and_why_others_were_passed_over() {
    let action = export_action(vec![interface_layer(), pixels_layer()]);

    // Standing inside a sandbox with no path to the app's interfaces: the
    // interface layer is passed over, the pixel layer is not.
    let (chosen, skipped) = action.plan(|layer| layer == Layer::Pixels);
    assert_eq!(
        chosen.map(|implementation| implementation.layer),
        Some(Layer::Pixels)
    );
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].layer, Layer::Interface);
    assert!(skipped[0].reason.contains("cannot be reached"));

    // With the app's interface on the bus, the precise layer wins and nothing
    // is passed over.
    let (chosen, skipped) = action.plan(|_| true);
    assert_eq!(
        chosen.map(|implementation| implementation.layer),
        Some(Layer::Interface)
    );
    assert!(skipped.is_empty());
}

/// An action whose implementations are all unreachable is refused with a
/// reason per layer, and one that declares none is refused for that.
#[test]
fn no_reachable_layer_is_a_reasoned_refusal() {
    let action = export_action(vec![interface_layer()]);
    let (chosen, skipped) = action.plan(|_| false);
    assert!(chosen.is_none());
    assert_eq!(
        skipped,
        vec![Skipped {
            layer: Layer::Interface,
            reason: "the interface layer cannot be reached here".into(),
        }]
    );

    let empty = export_action(Vec::new());
    let (chosen, skipped) = empty.plan(|_| true);
    assert!(chosen.is_none());
    assert_eq!(skipped[0].layer, Layer::Invalid);
}

/// Every implementation carries at least one testable check, and a claim
/// nothing can evaluate is refused: a layer that cannot say whether it worked
/// cannot outrank the one below it.
#[test]
fn every_implementation_needs_a_testable_check() {
    let unchecked = export_action(vec![implementation(
        Layer::Interface,
        vec![call_step()],
        vec![],
    )]);
    let errors = unchecked.check();
    assert!(
        errors.iter().any(|error| error.contains("no check")),
        "{errors:?}"
    );

    let untestable = export_action(vec![implementation(
        Layer::Accessibility,
        vec![Step::Invoke {
            role: "push button".into(),
            name: String::new(),
            action: "press".into(),
            value: None,
        }],
        vec![Check::Element {
            role: "dialog".into(),
            name: String::new(),
        }],
    )]);
    let errors = untestable.check();
    assert!(
        errors.iter().any(|error| error.contains("evaluate")),
        "{errors:?}"
    );
    assert!(
        !Check::File {
            path: "report.pdf".into(),
            bytes: 1
        }
        .is_testable()
    );

    // A sound action declares nothing to fix.
    assert!(export_action(vec![interface_layer()]).check().is_empty());
}

/// A step belongs to one layer, and an implementation that lets a pixel step
/// masquerade as an interface one is refused rather than silently planned
/// ahead of a precise layer.
#[test]
fn a_steps_layer_matches_the_implementation_it_sits_in() {
    let forgeries = [
        (call_step(), Layer::Interface),
        (
            Step::Edit {
                address: "tilo://notes/1".into(),
                edit: serde_json::json!({}),
            },
            Layer::Files,
        ),
        (
            Step::Invoke {
                role: "push button".into(),
                name: "Save".into(),
                action: "press".into(),
                value: None,
            },
            Layer::Accessibility,
        ),
        (Step::Click { x: 1, y: 2 }, Layer::Pixels),
    ];
    for (step, layer) in forgeries {
        assert_eq!(step.required_layer(), Some(layer));
        assert!(step.check().is_empty(), "{step:?} is well formed");
    }
}

/// The checks are what decides whether a layer worked, and silence is not
/// success: a check nothing observed is not satisfied.
#[test]
fn a_layers_checks_decide_whether_it_worked() {
    let pixels = implementation(
        Layer::Pixels,
        vec![Step::Press {
            keys: vec!["ctrl".into(), "s".into()],
        }],
        vec![check()],
    );
    assert!(!pixels.satisfied_by(&Observation::default()));

    let observed = Observation {
        files: vec![("/home/person/report.pdf".into(), 12_480)],
        ..Observation::default()
    };
    assert!(pixels.satisfied_by(&observed));

    // A different size is the dialog having saved something else.
    let wrong_size = Observation {
        files: vec![("/home/person/report.pdf".into(), 0)],
        ..Observation::default()
    };
    assert!(!pixels.satisfied_by(&wrong_size));
}

/// Undo is declared per action and reaches the contract a caller reads: one
/// nothing can take back is an action that must be approved before it runs.
#[test]
fn undo_kind_reaches_the_contract() {
    let mut action = export_action(vec![interface_layer()]);
    action.undo = Undo::None;
    action.contract.kind = ActionKind::Outside;
    let irreversible = adapter(vec![action]);
    let spec = irreversible.specs().pop().expect("one spec");
    assert!(!spec.contract.can_be_undone());
    assert!(spec.contract.tags.iter().any(|tag| tag == IRREVERSIBLE_TAG));

    let mut restorable = export_action(vec![interface_layer()]);
    restorable.undo = Undo::Restore;
    restorable.contract.kind = ActionKind::Outside;
    let restored = adapter(vec![restorable]);
    let spec = restored.specs().pop().expect("one spec");
    assert!(spec.contract.can_be_undone(), "{:?}", spec.contract);
}

/// An action that says it cannot be undone while its contract says Undo
/// takes it back is a contradiction, and an adapter that carries one is
/// planned nowhere — but its errors say what to repair.
#[test]
fn contradictory_adapters_are_invalid_and_say_why() {
    let mut action = export_action(vec![interface_layer()]);
    action.undo = Undo::None;
    let contradictory = adapter(vec![action]);
    assert_eq!(contradictory.usable_layer(), Layer::Invalid);
    let errors = contradictory.check();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("cannot be undone")),
        "{errors:?}"
    );

    let sound = adapter(vec![export_action(vec![interface_layer()])]);
    assert!(sound.check().is_empty());
    assert_eq!(sound.usable_layer(), Layer::Interface);
}

/// An adapter's actions are typed specs with their declared parameters, so a
/// Shortcut, a skill, an agent's catalogue and the palette reach them the way
/// they reach an app's own — and invoking one builds the adapter invocation.
#[test]
fn an_adapters_actions_are_typed_specs_that_build_invocations() {
    let mut action = export_action(vec![interface_layer()]);
    action.params = vec![ActionParam::required(
        "pages",
        ParamType::Text,
        "Which pages to export, as the document prints them",
    )];
    let adapter = adapter(vec![action]);

    let spec = adapter.specs().pop().expect("one spec");
    assert_eq!(spec.id, "export-pdf");
    assert_eq!(spec.params().len(), 1);
    assert_eq!(spec.params()[0].name, "pages");

    // The bind refuses a call that leaves the required argument out, before
    // any layer is reached.
    assert!(spec.build_args(Vec::new()).is_err());

    let encoded = spec
        .build_args([(
            "pages".to_string(),
            tilo_actions::params::ArgValue::Text("1-4".into()),
        )])
        .expect("bound");
    assert_eq!(encoded.type_id, ADAPTER_OPERATION_TYPE);
    let invocation = AdapterInvocation::from_encoded(&encoded).expect("an adapter invocation");
    assert_eq!(invocation.app, "org.example.Legacy");
    assert_eq!(invocation.action, "export-pdf");
    assert_eq!(
        invocation.arguments.get("pages"),
        Some(&serde_json::json!("1-4"))
    );
    assert!(invocation.missing_arguments(spec.params()).is_empty());

    let omitted =
        AdapterInvocation::new("org.example.Legacy", "export-pdf", serde_json::Map::new());
    assert_eq!(omitted.missing_arguments(spec.params()).len(), 1);
}

/// An adapter survives the trip to another collaborator unchanged: the
/// layers, the steps and the checks are the same values on arrival.
#[test]
fn an_adapter_round_trips_through_json() {
    let adapter = adapter(vec![export_action(vec![interface_layer(), pixels_layer()])]);
    let json = serde_json::to_value(&adapter).expect("serialised");
    let read: ActionAdapter = serde_json::from_value(json).expect("read back");
    assert_eq!(adapter, read);
    assert!(read.check().is_empty());
}

/// An adapter applies to the platform and app version it names, and to no
/// other: a recording is never replayed against an app it was not written
/// for.
#[test]
fn an_adapter_applies_only_to_what_it_names() {
    let adapter = adapter(vec![export_action(vec![interface_layer()])]);
    assert!(adapter.applies_to("linux", "24.2"));
    assert!(!adapter.applies_to("linux", "25.0"));
    assert!(!adapter.applies_to("web", "24.2"));

    // A pattern cannot be read into a claim, so it accepts nothing.
    let mut odd = adapter.clone();
    odd.app_version = "any version".into();
    assert!(!odd.applies_to("linux", "24.2"));

    let mut any = adapter;
    any.app_version = "any".into();
    assert!(any.applies_to("linux", "31.0"));
}

/// A step that names nothing is refused where it is declared rather than
/// discovered when the recording is replayed.
#[test]
fn a_step_that_cannot_run_is_refused() {
    let blank = Step::Call {
        bus: "org.example.Legacy".into(),
        path: "/org/example/Legacy".into(),
        interface: "org.example.Legacy.Documents".into(),
        method: String::new(),
        arguments: Vec::new(),
    };
    assert!(blank.check()[0].contains("method"));

    let insecure = Step::Request {
        method: "POST".into(),
        url: "ftp://example.org/export".into(),
        body: None,
    };
    assert_eq!(insecure.check().len(), 1);

    assert_eq!(Step::Press { keys: Vec::new() }.check().len(), 1);
    assert!(
        Step::Invoke {
            role: "push button".into(),
            name: "Save".into(),
            action: "press".into(),
            value: None,
        }
        .check()
        .is_empty()
    );
}
