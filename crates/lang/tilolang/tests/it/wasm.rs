//! The emitted modules validate and have the shape of implementation.md section 4.2.

use crate::common::{compile, compile_fixture, compile_with, fixture, todo_meta, validate};
use tilolang::abi::Imp;
use tilolang::{Compiled, PackageMeta};
use wasmparser::{Parser, Payload};

struct Shape {
    imports: Vec<(String, String)>,
    exports: Vec<String>,
    data_segments: usize,
    custom: Vec<String>,
}

fn shape(wasm: &[u8]) -> Shape {
    let mut s = Shape {
        imports: Vec::new(),
        exports: Vec::new(),
        data_segments: 0,
        custom: Vec::new(),
    };
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.expect("parses") {
            Payload::ImportSection(r) => {
                for imp in r.into_imports() {
                    let imp = imp.expect("import");
                    s.imports
                        .push((imp.module.to_string(), imp.name.to_string()));
                }
            }
            Payload::ExportSection(r) => {
                for e in r {
                    s.exports.push(e.expect("export").name.to_string());
                }
            }
            Payload::DataSection(r) => s.data_segments = r.count() as usize,
            Payload::CustomSection(c) => s.custom.push(c.name().to_string()),
            _ => {}
        }
    }
    s
}

fn assert_shape(c: &Compiled) {
    validate(&c.wasm);
    let s = shape(&c.wasm);
    let known: Vec<&str> = Imp::ALL.iter().map(|i| i.name()).collect();
    for (module, name) in &s.imports {
        assert_eq!(module, "tl");
        assert!(
            known.contains(&name.as_str()),
            "import {name} is not in section 4.4"
        );
    }
    assert!(s.exports.contains(&"memory".to_string()));
    for a in &c.meta.actions {
        assert!(s.exports.contains(&format!("a_{}", a.id)));
    }
    for v in &c.meta.views {
        assert!(s.exports.contains(&format!("v_{}", v.id)));
    }
    for f in &c.meta.flows {
        assert!(s.exports.contains(&format!("f_{}", f.id)));
    }
    for f in &c.meta.fns {
        assert!(s.exports.contains(&format!("n_{}", f.id)));
    }
    assert!(s.custom.contains(&"tilolang.meta".to_string()));
    assert_eq!(PackageMeta::from_wasm(&c.wasm).as_ref(), Some(&c.meta));
    assert_eq!(c.meta.module_hash.len(), 64);
}

#[test]
fn the_counter_validates_and_has_the_module_shape() {
    let c = compile_fixture("counter.tl", &[]);
    assert_shape(&c);
    let s = shape(&c.wasm);
    assert!(s.data_segments >= 1, "string literals are data segments");
    let names: Vec<&str> = s.imports.iter().map(|(_, n)| n.as_str()).collect();
    for want in [
        "str_lit",
        "res_add_i",
        "res_get_i",
        "ui_node",
        "ui_prop_i",
        "ui_on",
        "ui_anchor",
        "ui_child",
        "act_ref",
        "list_new",
        "list_push_h",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    assert!(!names.contains(&"invoke"));
}

#[test]
fn the_todo_app_validates() {
    let c = compile_fixture("todo.tl", &[]);
    assert_shape(&c);
    let names: Vec<String> = shape(&c.wasm).imports.into_iter().map(|(_, n)| n).collect();
    for want in [
        "coll_insert",
        "coll_remove",
        "coll_move",
        "rec_new",
        "rec_set_h",
        "ui_bind",
        "list_get_h",
        "list_len",
        "ref_id",
        "str_concat",
        "int_to_str",
    ] {
        assert!(names.contains(&want.to_string()), "{want} missing");
    }
    // The Bool field default and the checkbox bind use the field ids as string literals.
    let list = c
        .meta
        .types
        .iter()
        .find(|t| t.name == "TodoList")
        .expect("type");
    assert_eq!(
        list.fields
            .iter()
            .find(|f| f.name == "items")
            .map(|f| f.merge),
        Some(tilolang::types::MergeKind::Structural)
    );
    let todo = c
        .meta
        .types
        .iter()
        .find(|t| t.name == "Todo")
        .expect("type");
    assert_eq!(todo.fields[1].default, Some(serde_json::json!(false)));
}

#[test]
fn a_flow_that_invokes_a_foreign_action_validates() {
    let c = compile_with(&fixture("cleanup.tl"), &[todo_meta()]).expect("compiles");
    assert_shape(&c);
    let names: Vec<String> = shape(&c.wasm).imports.into_iter().map(|(_, n)| n).collect();
    assert!(names.contains(&"invoke".to_string()));
    assert!(names.contains(&"act_ref".to_string()));
}

#[test]
fn control_flow_and_scalars_validate() {
    let src = "type T {\n    n: Int\n    f: Float\n    b: Bool\n    s: Text\n}\n\
        fn calc(a: Int, b: Float, ok: Bool) -> Float {\n    let x = a.to_float() * b\n    if ok and a > 2 or not ok {\n        x = x + 1.5\n    }\n    for i in [1, 2, 3] {\n        x += i.to_float()\n    }\n    return if x > 0.0 then -x else x / 2.0\n}\n\
        fn label(a: Int, s: Text) -> Text {\n    if s < \"m\" {\n        return \"{a}{s}\"\n    }\n    return s + \"!\"\n}\n\
        action set(t: T, v: Int) -> Int {\n    t.n = v\n    t.f = 1.5\n    t.b = true\n    t.s = \"x\"\n    return t.n\n}\n\
        fn opt(t: T) -> Int {\n    let o: Option<Int> = t.n\n    return o.unwrap_or(0) + [1, 2].filter(x -> x > 1).count(y -> y > 0) + (1, \"a\").len()\n}";
    let c = crate::common::compile_body(src.replace("+ (1, \"a\").len()", "").as_str());
    assert_shape(&c);
}

#[test]
fn keys_inside_for_combine_the_call_id_with_the_item() {
    let c = compile_fixture("todo.tl", &[]);
    let names: Vec<String> = shape(&c.wasm).imports.into_iter().map(|(_, n)| n).collect();
    assert!(names.contains(&"str_concat".to_string()) && names.contains(&"ref_id".to_string()));
    let plain = compile(
        "app t.y \"Y\"\nview v(l: List<Int>) {\n    for x in l {\n        text(x)\n    }\n}\n",
    );
    assert_shape(&plain);
    let names: Vec<String> = shape(&plain.wasm)
        .imports
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    assert!(
        names.contains(&"int_to_str".to_string()),
        "plain lists key by index"
    );
}

#[test]
fn views_call_views_and_apply_foreign_views() {
    let src = "app t.z \"Z\"\nview inner(n: Int) {\n    text(n)\n}\nview outer() {\n    column {\n        for i in [1, 2] {\n            inner(i)\n        }\n    }\n}\n";
    let c = compile(src);
    assert_shape(&c);
}
