//! Outcomes (convergence design 1.1): action parameter defaults, evaluated when the action is
//! invoked without the argument.

use crate::common::{compile, validate};

const TODO: &str = "type Todo {\n    title: Text (title)\n    done: Bool = false\n}\n";

fn body(rest: &str) -> String {
    format!("app t.x \"X\"\n{TODO}{rest}\n")
}

const CHECK: &str = "/// Checks or unchecks an item.\naction check(item: Todo, done: Bool = not item.done) (label: if done { \"Check\" } else { \"Uncheck\" }, icon: .check, on: item.done) {\n    item.done = done\n}\n";

fn diagnostics(src: &str) -> Vec<tilolang::Diagnostic> {
    crate::common::compile_with(src, &[])
        .err()
        .unwrap_or_default()
}

#[test]
fn a_default_is_parsed_printed_and_described_in_the_metadata() {
    let src = body(CHECK);
    let c = compile(&src);
    let check = c
        .meta
        .actions
        .iter()
        .find(|a| a.name == "check")
        .expect("the action");
    assert_eq!(check.params.len(), 2);
    assert!(check.params[0].default.is_none(), "the subject has none");
    let done = check.params[1]
        .default
        .as_ref()
        .expect("`done` has a default");
    assert_eq!(done.text, "not item.done");
    assert_eq!(done.export, format!("d_{}_done", check.id));
    let exported = c
        .wasm
        .windows(done.export.len())
        .any(|w| w == done.export.as_bytes());
    assert!(exported, "the module exports the default's function");
    validate(&c.wasm);
    let printed = tilolang::print_module(&tilolang::parse(&src).expect("parses"));
    assert!(
        printed.contains("action check(item: Todo, done: Bool = not item.done) ("),
        "{printed}"
    );
}

#[test]
fn the_static_label_is_what_a_subject_at_rest_is_offered() {
    let c = compile(&body(CHECK));
    let check = c
        .meta
        .actions
        .iter()
        .find(|a| a.name == "check")
        .expect("the action");
    assert_eq!(
        check.label, "Check",
        "an item at rest is not done, so `done` defaults to true"
    );
    assert!(check.present.is_some(), "the label follows the item");
}

#[test]
fn a_default_reads_only_the_parameters_before_it() {
    let later =
        body("action a(item: Todo, flag: Bool = later, later: Bool) {\n    item.done = flag\n}");
    assert_eq!(codes_of(&later), vec!["E028"]);
    let itself = body("action a(item: Todo, flag: Bool = not flag) {\n    item.done = flag\n}");
    let d = diagnostics(&itself);
    assert_eq!(
        d.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(),
        vec!["E028"]
    );
    assert!(d[0].message.contains("reads itself"), "{}", d[0].message);
    let earlier = body(
        "action a(item: Todo, title: Text = item.title, again: Text = title) {\n    item.title = again\n}",
    );
    assert!(
        codes_of(&earlier).is_empty(),
        "an earlier parameter and its default are in scope"
    );
}

#[test]
fn a_default_is_pure() {
    let acts = body(&format!(
        "{CHECK}action a(item: Todo, x: Int = check(item)) {{\n    item.done = true\n}}"
    ));
    assert!(
        codes_of(&acts).contains(&"E043".to_owned()),
        "{:?}",
        codes_of(&acts)
    );
    let time = body("action a(item: Todo, at: Time = now) {\n    item.done = true\n}");
    assert_eq!(codes_of(&time), vec!["E049"]);
}

#[test]
fn only_an_action_takes_defaults() {
    for src in [
        "fn f(x: Int = 1) -> Int {\n    x\n}",
        "view v(item: Todo, x: Int = 1) {\n    text(item.title)\n}",
        "flow f(item: Todo, x: Int = 1) {\n}",
    ] {
        assert_eq!(codes_of(&body(src)), vec!["E028"], "{src}");
    }
    assert_eq!(
        codes_of(&body("enum E {\n    a(x: Int = 1)\n}")),
        vec!["E028"]
    );
}

#[test]
fn a_call_may_leave_out_a_defaulted_argument() {
    let views = body(&format!(
        "{CHECK}view v(item: Todo) {{\n    button(check(item))\n    button(check(item, done: false))\n}}"
    ));
    assert!(codes_of(&views).is_empty(), "{:?}", codes_of(&views));
    let actions = body(&format!(
        "{CHECK}action again(item: Todo) {{\n    check(item)\n    check(item, false)\n}}"
    ));
    assert!(codes_of(&actions).is_empty(), "{:?}", codes_of(&actions));
    validate(&compile(&actions).wasm);
    let flows = body(&format!(
        "{CHECK}flow f(item: Todo) {{\n    check(item)\n}}"
    ));
    assert!(codes_of(&flows).is_empty(), "{:?}", codes_of(&flows));
    let missing = body(&format!(
        "{CHECK}view v(item: Todo) {{\n    button(check())\n}}"
    ));
    assert_eq!(
        codes_of(&missing),
        vec!["E022"],
        "a parameter without a default is required"
    );
}

#[test]
fn an_applied_action_leaves_out_only_its_last_parameters() {
    let decl = "action change(item: Todo, done: Bool = not item.done, title: Text = item.title) {\n    item.done = done\n    item.title = title\n}\n";
    let gap = body(&format!(
        "{decl}view v(item: Todo) {{\n    button(change(item, title: \"x\"))\n}}"
    ));
    assert_eq!(codes_of(&gap), vec!["E028"]);
    let inline = body(&format!(
        "{decl}action a(item: Todo) {{\n    change(item, title: \"x\")\n}}"
    ));
    assert!(
        codes_of(&inline).is_empty(),
        "an inlined call computes the gap there"
    );
}

#[test]
fn a_label_reads_a_defaulted_parameter_but_not_one_without_a_default() {
    assert!(codes_of(&body(CHECK)).is_empty());
    let given = body(
        "action retitle(item: Todo, title: Text) (label: if title == \"\" { \"Clear\" } else { \"Rename\" }) {\n    item.title = title\n}",
    );
    assert_eq!(codes_of(&given), vec!["E090"]);
}

fn codes_of(src: &str) -> Vec<String> {
    diagnostics(src).into_iter().map(|d| d.code).collect()
}
