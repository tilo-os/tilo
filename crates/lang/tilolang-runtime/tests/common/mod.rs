//! Shared fixtures: a tiny WAT assembler over the `"tl"` ABI table, and the hand-written
//! modules the tests run. Replace `Meta` JSON here with the compiler's output when it lands.
#![allow(dead_code, reason = "each test file uses part of the fixtures")]

use std::fmt::Write as _;

use serde_json::{Value as Json, json};
use tilolang_runtime::abi::IMPORTS;
use tilolang_runtime::{
    AmStore, EngineLimits, Meta, PackageId, Principal, Runtime, SimpleAuthority, Value,
};

/// Builds a module: named string literals become `$L_<name>` functions returning a text
/// handle, and every `$tl_<import>` a function mentions is imported with its ABI signature.
#[derive(Default)]
pub struct Wat {
    lits: Vec<(String, String)>,
    funcs: Vec<String>,
}

fn wat_types(kind: &str, sig: &str) -> String {
    let mut out = String::new();
    for c in sig.chars() {
        write!(out, " ({kind} {})", wat_type(c)).unwrap();
    }
    out
}

fn wat_type(c: char) -> &'static str {
    match c {
        'l' => "i64",
        'f' => "f64",
        _ => "i32",
    }
}

impl Wat {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lit(mut self, name: &str, text: &str) -> Self {
        self.lits.push((name.to_owned(), text.to_owned()));
        self
    }

    pub fn func(mut self, wat: &str) -> Self {
        self.funcs.push(wat.to_owned());
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let body = self.funcs.join("\n");
        let mut src = String::from("(module\n");
        for (_, name, params, result) in IMPORTS {
            let used = body.contains(&format!("$tl_{name}"))
                || (*name == "str_lit" && !self.lits.is_empty());
            if !used {
                continue;
            }
            let p = wat_types("param", params);
            let r = wat_types("result", result);
            writeln!(src, "(import \"tl\" \"{name}\" (func $tl_{name}{p}{r}))").unwrap();
        }
        src.push_str("(memory (export \"memory\") 1)\n");
        let mut offset = 0usize;
        for (name, text) in &self.lits {
            writeln!(src, "(data (i32.const {offset}) \"{text}\")").unwrap();
            writeln!(
                src,
                "(func $L_{name} (result i32) (call $tl_str_lit (i32.const {offset}) (i32.const {})))",
                text.len()
            )
            .unwrap();
            offset = offset.saturating_add(text.len());
        }
        src.push_str(&body);
        src.push_str("\n)");
        wat::parse_str(&src).unwrap_or_else(|e| panic!("bad wat: {e}\n{src}"))
    }
}

pub fn meta(v: Json) -> Meta {
    serde_json::from_value(v).unwrap()
}

pub fn me() -> Principal {
    Principal::new("me")
}

/// A view that shows a counter and a "+1" button whose press is the action `inc`.
pub fn counter_view(export: &str) -> String {
    format!(
        r#"(func (export "{export}") (param $c i32) (result i32)
  (local $col i32) (local $txt i32) (local $btn i32) (local $args i32)
  (local.set $col (call $tl_ui_node (call $L_column) (call $L_kcol)))
  (local.set $txt (call $tl_ui_node (call $L_text) (call $L_ktxt)))
  (call $tl_ui_prop_i (local.get $txt) (call $L_text) (call $tl_res_get_i (local.get $c) (call $L_fv)))
  (local.set $btn (call $tl_ui_node (call $L_button) (call $L_kbtn)))
  (call $tl_ui_prop_h (local.get $btn) (call $L_label) (call $L_plus1))
  (local.set $args (call $tl_list_new))
  (call $tl_list_push_h (local.get $args) (local.get $c))
  (call $tl_ui_on (local.get $btn) (call $L_press) (call $tl_act_ref (call $L_inc) (local.get $args)))
  (call $tl_ui_child (local.get $col) (local.get $txt))
  (call $tl_ui_child (local.get $col) (local.get $btn))
  (local.get $col))"#
    )
}

/// A flow that invokes `inc` twice on its counter and returns the counter's value.
pub fn twice_flow(export: &str) -> String {
    format!(
        r#"(func (export "{export}") (param $c i32) (result i64)
  (local $args i32) (local $a i32)
  (local.set $args (call $tl_list_new))
  (call $tl_list_push_h (local.get $args) (local.get $c))
  (local.set $a (call $tl_act_ref (call $L_inc) (local.get $args)))
  (drop (call $tl_invoke (local.get $a)))
  (drop (call $tl_invoke (local.get $a)))
  (call $tl_res_get_i (local.get $c) (call $L_fv)))"#
    )
}

/// The literals the counter views and flows use.
pub fn counter_lits(w: Wat) -> Wat {
    w.lit("fv", "fv")
        .lit("inc", "inc")
        .lit("column", "column")
        .lit("text", "text")
        .lit("button", "button")
        .lit("kcol", "col")
        .lit("ktxt", "txt")
        .lit("kbtn", "btn")
        .lit("label", "label")
        .lit("plus1", "+1")
        .lit("press", "press")
        .lit("minrem", "min.rem")
        .lit("gap", "gap")
        .lit("section", "section")
        .lit("compact", "compact")
}

pub fn counter_wasm() -> Vec<u8> {
    counter_lits(Wat::new())
        .func(
            r#"(func (export "a_inc") (param $c i32)
  (call $tl_res_add_i (local.get $c) (call $L_fv) (i64.const 1)))"#,
        )
        .func(
            r#"(func (export "a_bad") (param $c i32)
  (call $tl_res_add_i (local.get $c) (call $L_fv) (i64.const 1))
  unreachable)"#,
        )
        .func(r#"(func (export "a_spin") (param $c i32) (loop $l (br $l)))"#)
        .func(r#"(func (export "a_grow") (param $c i32) (drop (memory.grow (i32.const 4096))))"#)
        .func(r#"(func (export "a_forge") (param $c i32) (drop (call $tl_str_len (i32.const 12345))))"#)
        .func(&counter_view("v_vc"))
        .func(
            r#"(func (export "v_vmut") (param $c i32) (result i32)
  (call $tl_res_add_i (local.get $c) (call $L_fv) (i64.const 1))
  (call $tl_ui_node (call $L_column) (call $L_kcol)))"#,
        )
        .func(&twice_flow("f_twice"))
        .func(
            r#"(func (export "v_vspace") (param $c i32) (result i32) (local $n i32)
  (local.set $n (call $tl_ui_node (call $L_column) (call $L_kcol)))
  (call $tl_ui_prop_f (local.get $n) (call $L_minrem) (f64.const 12.5))
  (call $tl_ui_prop_h (local.get $n) (call $L_gap) (call $L_section))
  (call $tl_ui_prop_i (local.get $n) (call $L_compact) (call $tl_space_i (call $L_compact)))
  (local.get $n))"#,
        )
        .build()
}

fn action(id: &str, name: &str, class: &str, params: &Json) -> Json {
    json!({"id": id, "name": name, "class": class, "params": params})
}

pub fn counter_meta() -> Meta {
    let c = json!([{"name": "c", "type": "Counter"}]);
    meta(json!({
        "app": "so.tilo.counter", "title": "Counter",
        "types": [{"id": "tc", "name": "Counter", "doc": "A counter.",
                   "fields": [{"id": "fv", "name": "value", "type": "Int", "merge": "counter"}]}],
        "actions": [
            {"id": "inc", "name": "increment", "doc": "Add one to the counter.", "class": "undoable", "params": c},
            action("bad", "bad", "undoable", &c),
            action("spin", "spin", "undoable", &c),
            action("grow", "grow", "undoable", &c),
            action("forge", "forge", "undoable", &c),
        ],
        "views": [
            {"id": "vc", "name": "counter", "params": c, "anchors": [{"id": "p", "name": "plus"}]},
            {"id": "vmut", "name": "mutating", "params": c},
            {"id": "vspace", "name": "spacey", "params": c},
        ],
        "flows": [{"id": "twice", "name": "twice", "params": c, "result": "Int"}],
    }))
}

/// A runtime over `store` with the counter package loaded.
pub struct World {
    pub rt: Runtime,
    pub store: AmStore,
    pub auth: SimpleAuthority,
    pub counter: PackageId,
    pub me: Principal,
}

impl World {
    pub fn new() -> Self {
        Self::with(&AmStore::new(), EngineLimits::default())
    }

    pub fn with(store: &AmStore, limits: EngineLimits) -> Self {
        let auth = SimpleAuthority::new();
        let rt = Runtime::with_limits(store.clone(), auth.clone(), limits).unwrap();
        let counter = rt.load_package(&counter_wasm(), counter_meta()).unwrap();
        Self {
            rt,
            store: store.clone(),
            auth,
            counter,
            me: me(),
        }
    }

    pub fn counter(&self) -> tilolang_runtime::ResourceRef {
        self.rt.create("Counter").unwrap()
    }

    pub fn inc(&self, c: &tilolang_runtime::ResourceRef) {
        self.rt
            .invoke_action(&self.me, ("inc", vec![Value::resource(c)]))
            .unwrap();
    }
}

/// A runtime over `store` with the given packages loaded.
pub fn replica(store: &AmStore, packages: &[(Vec<u8>, Meta)]) -> Runtime {
    let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
    for (wasm, m) in packages {
        rt.load_package(wasm, m.clone()).unwrap();
    }
    rt
}

pub fn note_wasm() -> Vec<u8> {
    Wat::new()
        .lit("fb", "fb")
        .lit("input", "input")
        .lit("kin", "in")
        .lit("text", "text")
        .func(
            r#"(func (export "a_set_body") (param $n i32) (param $s i32)
  (call $tl_res_text_set (local.get $n) (call $L_fb) (local.get $s)))"#,
        )
        .func(
            r#"(func (export "v_vn") (param $n i32) (result i32) (local $in i32)
  (local.set $in (call $tl_ui_node (call $L_input) (call $L_kin)))
  (call $tl_ui_prop_h (local.get $in) (call $L_text) (call $tl_res_get_h (local.get $n) (call $L_fb)))
  (call $tl_ui_bind (local.get $in) (local.get $n) (call $L_fb))
  (local.get $in))"#,
        )
        .build()
}

pub fn note_meta() -> Meta {
    meta(json!({
        "app": "so.tilo.note",
        "types": [{"id": "tn", "name": "Note",
                   "fields": [{"id": "fb", "name": "body", "type": "Text"}]}],
        "actions": [{"id": "set_body", "name": "set_body", "class": "undoable",
                     "params": [{"name": "n", "type": "Note"}, {"name": "s", "type": "Text"}]}],
        "views": [{"id": "vn", "name": "note", "params": [{"name": "n", "type": "Note"}]}],
    }))
}

pub fn list_wasm() -> Vec<u8> {
    Wat::new()
        .lit("fi", "fi")
        .lit("fl", "fl")
        .lit("ti", "ti")
        .lit("column", "column")
        .lit("text", "text")
        .lit("kcol", "col")
        .func(
            r#"(func (export "a_add") (param $l i32) (param $s i32) (local $r i32)
  (local.set $r (call $tl_rec_new (call $L_ti)))
  (call $tl_rec_set_h (local.get $r) (call $L_fl) (local.get $s))
  (drop (call $tl_coll_insert (local.get $l) (call $L_fi) (local.get $r))))"#,
        )
        .func(
            r#"(func (export "a_move_first") (param $l i32) (param $it i32)
  (call $tl_coll_move (local.get $l) (call $L_fi) (local.get $it) (i64.const 0)))"#,
        )
        .func(
            r#"(func (export "a_drop") (param $l i32) (param $it i32)
  (call $tl_coll_remove (local.get $l) (call $L_fi) (local.get $it)))"#,
        )
        .func(
            r#"(func (export "v_vl") (param $l i32) (result i32)
  (local $col i32) (local $items i32) (local $i i64) (local $n i64) (local $it i32) (local $t i32)
  (local.set $col (call $tl_ui_node (call $L_column) (call $L_kcol)))
  (local.set $items (call $tl_res_get_h (local.get $l) (call $L_fi)))
  (local.set $n (call $tl_list_len (local.get $items)))
  (block $done
    (loop $next
      (br_if $done (i64.ge_s (local.get $i) (local.get $n)))
      (local.set $it (call $tl_list_get_h (local.get $items) (local.get $i)))
      (local.set $t (call $tl_ui_node (call $L_text) (call $tl_ref_id (local.get $it))))
      (call $tl_ui_prop_h (local.get $t) (call $L_text) (call $tl_res_get_h (local.get $it) (call $L_fl)))
      (call $tl_ui_child (local.get $col) (local.get $t))
      (local.set $i (i64.add (local.get $i) (i64.const 1)))
      (br $next)))
  (local.get $col))"#,
        )
        .build()
}

pub fn list_meta() -> Meta {
    let l = json!({"name": "l", "type": "List"});
    meta(json!({
        "app": "so.tilo.list",
        "types": [
            {"id": "tl", "name": "List",
             "fields": [{"id": "fi", "name": "items", "type": "Collection<Item>"}]},
            {"id": "ti", "name": "Item",
             "fields": [{"id": "fl", "name": "label", "type": "Text"}]},
        ],
        "actions": [
            {"id": "add", "name": "add", "class": "undoable",
             "params": [l, {"name": "s", "type": "Text"}]},
            {"id": "move_first", "name": "move_first", "class": "undoable",
             "params": [l, {"name": "it", "type": "Item"}]},
            {"id": "drop", "name": "drop", "class": "undoable",
             "params": [l, {"name": "it", "type": "Item"}]},
        ],
        "views": [{"id": "vl", "name": "list", "params": [l]}],
    }))
}

/// The labels of a list's items, in order.
pub fn labels(rt: &Runtime, list: &tilolang_runtime::ResourceRef) -> Vec<String> {
    let Value::List(items) = rt.read(list, "items").unwrap() else {
        panic!("items is not a list");
    };
    items
        .iter()
        .map(|i| {
            let Value::Ref(r) = i else {
                panic!("not a ref")
            };
            rt.read(&r.target, "label")
                .unwrap()
                .as_text()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The item of `list` labelled `label`.
pub fn item(rt: &Runtime, list: &tilolang_runtime::ResourceRef, label: &str) -> Value {
    let Value::List(items) = rt.read(list, "items").unwrap() else {
        panic!("items is not a list");
    };
    items
        .into_iter()
        .find(|i| {
            let Value::Ref(r) = i else { return false };
            rt.read(&r.target, "label").unwrap().as_text() == Some(label)
        })
        .unwrap_or_else(|| panic!("no item {label}"))
}

/// A module by someone else: a view whose button invokes the counter app's `inc`, and flows
/// that invoke it, one of which calls a model first.
pub fn stranger_wasm() -> Vec<u8> {
    counter_lits(Wat::new())
        .lit("kind", "classify")
        .lit("input", "hello")
        .func(&counter_view("v_vs"))
        .func(&twice_flow("f_ask"))
        .func(
            r#"(func (export "f_m") (param $c i32) (result i32)
  (local $m i32) (local $args i32)
  (local.set $m (call $tl_model_call (call $L_kind) (call $L_input)))
  (local.set $args (call $tl_list_new))
  (call $tl_list_push_h (local.get $args) (local.get $c))
  (drop (call $tl_invoke (call $tl_act_ref (call $L_inc) (local.get $args))))
  (local.get $m))"#,
        )
        .build()
}

pub fn stranger_meta() -> Meta {
    let c = json!([{"name": "c", "type": "Counter"}]);
    meta(json!({
        "app": "x.stranger",
        "views": [{"id": "vs", "name": "strange", "params": c}],
        "flows": [
            {"id": "ask", "name": "ask", "params": c, "result": "Int"},
            {"id": "m", "name": "m", "params": c, "result": "Text"},
        ],
    }))
}

pub fn mail_wasm() -> Vec<u8> {
    Wat::new()
        .lit("ft", "ft")
        .lit("post", "post")
        .func(
            r#"(func (export "a_set_to") (param $m i32) (param $s i32)
  (call $tl_res_text_set (local.get $m) (call $L_ft) (local.get $s)))"#,
        )
        .func(
            r#"(func (export "a_send") (param $m i32)
  (drop (call $tl_outside (call $L_post) (call $tl_res_get_h (local.get $m) (call $L_ft)))))"#,
        )
        .func(
            r#"(func (export "a_send_bad") (param $m i32)
  (drop (call $tl_outside (call $L_post) (call $tl_res_get_h (local.get $m) (call $L_ft))))
  unreachable)"#,
        )
        .func(
            r#"(func (export "a_sneak") (param $m i32)
  (drop (call $tl_outside (call $L_post) (call $tl_res_get_h (local.get $m) (call $L_ft)))))"#,
        )
        .build()
}

pub fn mail_meta() -> Meta {
    let m = json!([{"name": "m", "type": "Mail"}]);
    meta(json!({
        "app": "so.tilo.mail",
        "types": [{"id": "tm", "name": "Mail",
                   "fields": [{"id": "ft", "name": "to", "type": "Text"}]}],
        "actions": [
            {"id": "set_to", "name": "set_to", "class": "undoable",
             "params": [{"name": "m", "type": "Mail"}, {"name": "s", "type": "Text"}]},
            {"id": "send", "name": "send", "class": "outside", "params": m},
            {"id": "send_bad", "name": "send_bad", "class": "outside", "params": m},
            {"id": "sneak", "name": "sneak", "class": "undoable", "params": m},
        ],
    }))
}
