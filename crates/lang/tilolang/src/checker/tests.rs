//! The checker's own invariants: a prop it sets on a described component that the component does
//! not declare, and the diagnostic for a type the package gives no default view.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert by panicking"
)]

use crate::abi::Imp;
use crate::ids::{IdsLock, SeededEntropy, declared_names};
use crate::ir::{Expr, Func, Module, Stmt};

/// A row opens the resource it presents in the type's default view; a type the package gives none
/// opens as the platform's page of its fields, which the checker says out loud, with both fixes.
#[test]
fn opening_a_type_with_no_default_view_says_what_to_add() {
    let page = "type Note {\n    title: Text\n}\ntype Notebook {\n    notes: Collection<Note>\n}\nroot Notebook\ndefault view notebook for Notebook\n";
    let views = "view notebook(nb: Notebook) {\n    for n in nb.notes {\n        ui.ListRow(n.title, item: n) {\n            on activate: open(n)\n        }\n    }\n}\nview note(n: Note) {\n    ui.ListRow(n.title)\n}";
    let named = diagnostics(&format!("{page}default view note for Note\n{views}"));
    assert!(named.is_empty(), "{named:#?}");
    let missing = diagnostics(&format!("{page}{views}"));
    let codes: Vec<&str> = missing.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["E141"], "{missing:#?}");
    let message = &missing[0].message;
    assert!(
        message.contains("default view <view> for Note"),
        "{message}"
    );
    assert!(message.contains("open(r, view: <view>)"), "{message}");
}

fn diagnostics(body: &str) -> Vec<crate::diag::Diagnostic> {
    let src = format!("app t.x \"X\"\n{body}\n");
    let (module, diags) = crate::parser::parse_module(&src);
    assert!(diags.is_empty(), "{diags:#?}");
    let module = module.expect("parses");
    let mut lock = IdsLock::new();
    lock.sync(&declared_names(&module), &mut SeededEntropy(3));
    let mut diags = Vec::new();
    let env = crate::resolve::resolve(&module, &lock, &[], &mut diags);
    let _ = super::check_module(&env, &module, &mut diags);
    diags
}

/// A prop the checker sets on a described component that the component does not declare is the
/// compiler's own defect: the window refuses a batch that sets one, so the compile that would
/// have sent it has to fail first.
#[test]
fn a_prop_a_component_does_not_declare_fails_the_compile() {
    let module = |props: &[(&str, &str)]| {
        let node = Expr::host(
            Imp::UiNode,
            vec![Expr::str("ui.ListRow"), Expr::str("row")],
            crate::types::Ty::View,
        );
        let mut body = vec![Stmt::Set(0, node)];
        for (name, value) in props {
            body.push(Stmt::Eval(Expr::host(
                Imp::UiPropH,
                vec![
                    Expr::local(0, crate::types::Ty::View),
                    Expr::str(name),
                    Expr::str(value),
                ],
                crate::types::Ty::Unit,
            )));
        }
        Module {
            funcs: vec![Func {
                at: None,
                kind: crate::ir::FuncKind::View,
                name: "task_row".into(),
                export: "v_task_row".into(),
                nparams: 0,
                result: crate::types::Ty::View,
                locals: vec![crate::types::Ty::View],
                body,
                facts: crate::ir::Facts::default(),
            }],
            ..Module::default()
        }
    };
    let declared = [
        ("title", "Buy oat milk"),
        ("meta", "2026-11-03"),
        ("meta:as", "date"),
        ("meta:time", "2026-11-03T17:00:00Z"),
        ("meta_tone", "warning"),
    ];
    let mut diags = Vec::new();
    super::props::undeclared(&module(&declared), &mut diags);
    assert!(diags.is_empty(), "{diags:#?}");
    let mut diags = Vec::new();
    super::props::undeclared(&module(&[("meta:tmie", "2026-11-03")]), &mut diags);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "E000");
    assert!(diags[0].message.contains("meta:tmie"), "{diags:#?}");
}

/// An `on begin` handler says the field its made record is edited at (`binds .title`): refused
/// without one, since the compiler can only guess, and a guess binds the wrong field.
#[test]
fn a_begin_handler_without_a_binds_clause_is_refused() {
    let body = "type Step {\n    title: Text\n    done: Bool\n}\n\
                action make(s: Step, title: Text) -> Step {\n    s.title = title\n    s\n}\n\
                view editor(s: Step) {\n    ui.TextInput(placeholder: \"Add\") {\n        \
                on begin(text: Text): make(s, text)\n    }\n}\n";
    let diags = diagnostics(body);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "E063" && d.message.contains("binds .title")),
        "{diags:#?}"
    );
}

/// A `binds` field that is not text on the made type is refused: the field is what a person types
/// into, so it is a `Text` field of the returned record.
#[test]
fn a_begin_handler_binds_only_a_text_field_of_the_made_type() {
    let body = "type Step {\n    title: Text\n    done: Bool\n}\n\
                action make(s: Step, title: Text) -> Step {\n    s.title = title\n    s\n}\n\
                view editor(s: Step) {\n    ui.TextInput(placeholder: \"Add\") {\n        \
                on begin(text: Text): make(s, text) binds .done\n    }\n}\n";
    let diags = diagnostics(body);
    assert!(
        diags
            .iter()
            .any(|d| d.code == "E063" && d.message.contains("not text")),
        "{diags:#?}"
    );
}

/// The clause the checker asks for: a text field of the returned record is accepted.
#[test]
fn a_begin_handler_binding_a_text_field_is_accepted() {
    let body = "type Step {\n    title: Text\n    done: Bool\n}\n\
                action make(s: Step, title: Text) -> Step {\n    s.title = title\n    s\n}\n\
                view editor(s: Step) {\n    ui.TextInput(placeholder: \"Add\") {\n        \
                on begin(text: Text): make(s, text) binds .title\n    }\n}\n";
    let diags = diagnostics(body);
    assert!(diags.is_empty(), "{diags:#?}");
}
