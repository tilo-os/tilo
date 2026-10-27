//! From a declared adapter action to the work that would perform it: the
//! arguments in place, the layer chosen, and nothing a parameter can bend
//! into a command or a different bus object.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::json;
use tilo_actions::adapter::{
    ADAPTER_OPERATION_TYPE, ActionAdapter, AdapterAction, AdapterInvocation, Check, Implementation,
    Layer, Step, Undo,
};
use tilo_actions::adapter_run::{
    BusCall, Plan, Refusal, Work, invocation_of, is_bus_name, is_bus_value,
};
use tilo_actions::contract::ActionKind;
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::{ActionContract, EncodedAction, IRREVERSIBLE_TAG};

fn param(name: &str, ty: ParamType, required: bool) -> ActionParam {
    ActionParam {
        name: Cow::Owned(name.to_owned()),
        ty,
        required,
        description: Cow::Owned(format!("{name} of the action")),
        choices: Vec::new(),
        schema: None,
        nullable: false,
    }
}

fn call_step(arguments: Vec<serde_json::Value>) -> Step {
    Step::Call {
        bus: "org.freedesktop.portal.Screenshot".into(),
        path: "/org/freedesktop/portal/desktop".into(),
        interface: "org.freedesktop.portal.Screenshot".into(),
        method: "PickColor".into(),
        arguments,
    }
}

fn run_step(program: &str, args: &[&str]) -> Step {
    Step::Run {
        program: program.into(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        stdin: None,
    }
}

fn check() -> Check {
    Check::File {
        path: "/home/person/shots/colour.png".into(),
        bytes: 4_096,
    }
}

fn implementation(layer: Layer, steps: Vec<Step>) -> Implementation {
    Implementation {
        layer,
        steps,
        checks: vec![check()],
        app_version: "any".into(),
        recorded_from: None,
    }
}

fn action(
    id: &str,
    params: Vec<ActionParam>,
    implementations: Vec<Implementation>,
) -> AdapterAction {
    AdapterAction {
        id: id.into(),
        label: id.into(),
        keywords: Vec::new(),
        params,
        contract: ActionContract::default(),
        undo: Undo::Restore,
        implementations,
    }
}

fn adapter(actions: Vec<AdapterAction>) -> ActionAdapter {
    ActionAdapter {
        app: "org.freedesktop.portal.Desktop".into(),
        app_version: "any".into(),
        platform: "linux".into(),
        description: String::new(),
        actions,
    }
}

fn bound(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect::<BTreeMap<String, serde_json::Value>>()
        .into_iter()
        .collect()
}

/// The acceptance: a Flatpak app's D-Bus method becomes one typed bus call,
/// with the argument the caller bound, and the check the declaration states.
#[test]
fn a_dbus_method_lowers_to_one_typed_call_with_the_bound_argument() {
    let action = action(
        "pick-colour",
        vec![param("handle", ParamType::Text, true)],
        vec![implementation(
            Layer::Interface,
            vec![call_step(vec![json!("h-1")])],
        )],
    );
    let adapter = adapter(vec![action.clone()]);
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "pick-colour",
        bound(&[("handle", json!("h-1"))]),
    );
    let plan = Plan::of(&action, &invocation, |_| true).expect("a plan");

    assert_eq!(plan.layer, Layer::Interface);
    assert_eq!(plan.checks, vec![check()]);
    assert_eq!(plan.undo, Undo::Restore);
    let calls = plan.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        &BusCall {
            bus: "org.freedesktop.portal.Screenshot".into(),
            path: "/org/freedesktop/portal/desktop".into(),
            interface: "org.freedesktop.portal.Screenshot".into(),
            method: "PickColor".into(),
            arguments: vec![json!("h-1")],
        }
    );
    assert_eq!(
        calls[0].to_request(),
        vec![
            "org.freedesktop.portal.Screenshot",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Screenshot",
            "PickColor",
            "--",
            "\"h-1\"",
        ]
    );

    // The spec the adapter builds is the one a Shortcut, a palette or an
    // agent reach; invoking it encodes the same invocation this planned.
    let specs = adapter.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].id, "pick-colour");
    assert_eq!(specs[0].params().len(), 1);
}

/// The CLI half: a command line is a program and its argument vector, and a
/// parameter's value is one argument, whatever it contains.
#[test]
fn a_command_lowers_to_a_program_and_its_argument_vector() {
    let action = action(
        "resize",
        vec![
            param("width", ParamType::Integer, true),
            param("path", ParamType::Text, true),
        ],
        vec![implementation(
            Layer::Interface,
            vec![run_step(
                "magick",
                &[
                    "convert",
                    "{{param.path}}",
                    "-resize",
                    "{{param.width}}x",
                    "out.png",
                ],
            )],
        )],
    );
    // A path that would change what a shell did is still one argument.
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "resize",
        bound(&[("width", json!(640)), ("path", json!("a; rm -rf ~/notes"))]),
    );
    let plan = Plan::of(&action, &invocation, |_| true).expect("a plan");
    let runs: Vec<(&str, Vec<&str>)> = plan
        .runs()
        .into_iter()
        .map(|(program, args)| (program, args.iter().map(String::as_str).collect()))
        .collect();
    assert_eq!(
        runs,
        vec![(
            "magick",
            vec!["convert", "a; rm -rf ~/notes", "-resize", "640x", "out.png"]
        )]
    );
    assert!(plan.calls().is_empty());
    assert!(plan.runs_only());
}

/// A layer that cannot be reached here is not planned at all, and the plan
/// says which ones were passed over and why.
#[test]
fn a_plan_is_refused_when_no_layer_is_reachable() {
    let action = action(
        "resize",
        Vec::new(),
        vec![implementation(
            Layer::Interface,
            vec![call_step(Vec::new())],
        )],
    );
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "resize",
        serde_json::Map::new(),
    );
    let refused = Plan::of(&action, &invocation, |_| false).expect_err("no layer");
    let Refusal::NoReachableLayer(skipped) = refused else {
        panic!("expected skipped layers, got {refused:?}");
    };
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].layer, Layer::Interface);
    assert!(skipped[0].reason.contains("cannot be reached"));
}

/// A required parameter the caller left out never reaches a program: the
/// refusal names it first, as the bind of a typed action already does.
#[test]
fn a_missing_argument_refuses_the_plan_before_any_work_is_built() {
    let action = action(
        "resize",
        vec![param("path", ParamType::Text, true)],
        vec![implementation(
            Layer::Interface,
            vec![run_step("magick", &[])],
        )],
    );
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "resize",
        serde_json::Map::new(),
    );
    let refused = Plan::of(&action, &invocation, |_| true).expect_err("missing");
    let Refusal::Missing(missing) = refused else {
        panic!("expected a missing argument, got {refused:?}");
    };
    assert_eq!(missing, vec!["`path` is required".to_owned()]);
}

/// A declaration that spells its bus name, path, interface or method as
/// something else is refused rather than handed to a bus client: the parts
/// of a call are each the kind of name they claim to be.
#[test]
fn a_call_whose_parts_are_not_names_is_refused() {
    for (name, step) in [
        (
            "bus",
            Step::Call {
                bus: "org.freedesktop.portal.Screenshot; sh".into(),
                path: "/org/freedesktop/portal/desktop".into(),
                interface: "org.freedesktop.portal.Screenshot".into(),
                method: "PickColor".into(),
                arguments: Vec::new(),
            },
        ),
        (
            "path",
            Step::Call {
                bus: "org.freedesktop.portal.Screenshot".into(),
                path: "/org/freedesktop/../etc".into(),
                interface: "org.freedesktop.portal.Screenshot".into(),
                method: "PickColor".into(),
                arguments: Vec::new(),
            },
        ),
        (
            "interface",
            Step::Call {
                bus: "org.freedesktop.portal.Screenshot".into(),
                path: "/org/freedesktop/portal/desktop".into(),
                interface: "org.freedesktop.portal.Screenshot()".into(),
                method: "PickColor".into(),
                arguments: Vec::new(),
            },
        ),
        (
            "method",
            Step::Call {
                bus: "org.freedesktop.portal.Screenshot".into(),
                path: "/org/freedesktop/portal/desktop".into(),
                interface: "org.freedesktop.portal.Screenshot".into(),
                method: "PickColor --all".into(),
                arguments: Vec::new(),
            },
        ),
    ] {
        let action = action(
            "pick-colour",
            Vec::new(),
            vec![implementation(Layer::Interface, vec![step.clone()])],
        );
        let invocation = AdapterInvocation::new(
            "org.freedesktop.portal.Desktop",
            "pick-colour",
            serde_json::Map::new(),
        );
        let refused = Plan::of(&action, &invocation, |_| true).expect_err(name);
        assert!(
            matches!(refused, Refusal::Unsupported { step: "call", .. }),
            "{name}: {refused:?}"
        );
    }
}

/// UI layers are not performed by this lowering: an adapter's interface
/// layer is what it declares, and a pixel or accessibility step is reached
/// by the layer that drives the app's window.
#[test]
fn ui_steps_are_not_performed_as_interface_work() {
    let action = action(
        "export",
        Vec::new(),
        vec![implementation(
            Layer::Pixels,
            vec![Step::Click { x: 10, y: 20 }],
        )],
    );
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "export",
        serde_json::Map::new(),
    );
    let refused = Plan::of(&action, &invocation, |_| true).expect_err("pixels");
    let Refusal::Unsupported { step, .. } = refused else {
        panic!("expected an unsupported step, got {refused:?}");
    };
    assert_eq!(step, "pixels");
}

/// An invocation is read back from the encoded operation an executor was
/// handed, and an operation of another type is refused.
#[test]
fn an_invocation_is_read_back_from_what_an_executor_is_handed() {
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "pick-colour",
        bound(&[("handle", json!("h-1"))]),
    );
    let encoded = invocation.encode();
    assert_eq!(encoded.type_id, ADAPTER_OPERATION_TYPE);
    assert_eq!(invocation_of(&encoded).expect("an invocation"), invocation);

    let other = EncodedAction::encode_operation("tilo.other", &json!({})).expect("encoded");
    let refused = invocation_of(&other).expect_err("not an adapter");
    assert_eq!(refused, Refusal::NotAnAdapter("tilo.other".into()));
}

/// What Undo means is carried onto the plan, so a host that saves the
/// no-undo case before it runs the work.
#[test]
fn the_plan_carries_what_undo_means() {
    let mut without_undo = action(
        "delete-all",
        Vec::new(),
        vec![implementation(
            Layer::Interface,
            vec![call_step(Vec::new())],
        )],
    );
    without_undo.undo = Undo::None;
    without_undo.contract = ActionContract {
        kind: ActionKind::Outside,
        tags: vec![IRREVERSIBLE_TAG.to_owned()],
        ..ActionContract::default()
    };
    assert!(without_undo.check().is_empty(), "the declaration is sound");
    let invocation = AdapterInvocation::new(
        "org.freedesktop.portal.Desktop",
        "delete-all",
        serde_json::Map::new(),
    );
    let plan = Plan::of(&without_undo, &invocation, |_| true).expect("a plan");
    assert_eq!(plan.undo, Undo::None);
    assert!(matches!(plan.steps[0], Work::Call(_)));
}

/// The two vocabularies a bus takes: names and values.
#[test]
fn bus_names_and_carriable_values_are_read_as_their_grammar_allows() {
    assert!(is_bus_name("org.freedesktop.portal.Screenshot"));
    assert!(!is_bus_name("org"));
    assert!(is_bus_value(ParamType::Text));
    assert!(is_bus_value(ParamType::Object));
    assert!(!is_bus_value(ParamType::Secret));
    assert!(!is_bus_value(ParamType::Resource));
}
