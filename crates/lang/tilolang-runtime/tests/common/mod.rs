//! Shared fixtures: a tiny WAT assembler over the `"tl"` ABI table, and the hand-written
//! modules the tests run. Replace `Meta` JSON here with the compiler's output when it lands.
#![allow(dead_code, reason = "each test file uses part of the fixtures")]

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::abi::IMPORTS;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::ids::RunId;
use tilolang_runtime::{
    ActionInfo, Actor, AmStore, Authority, DEFAULT_LIMITS, Decision, EngineLimits, InvokeOutcome,
    Meta, ModuleHash, PackageId, RunOutcome, Runtime, SimpleAuthority, Value,
};

/// The Tasks package (`apps/tl/tasks`): its app and the scenarios staged beside it.
pub const TASKS_SOURCE: &str = concat!(
    include_str!("../../../../../apps/tl/tasks/app.tl"),
    "\n",
    include_str!("../../../../../apps/tl/tasks/scenarios.tl")
);

#[derive(Default)]
struct Overrides {
    denied_modules: std::collections::HashSet<ModuleHash>,
    denied_actions: std::collections::HashSet<String>,
    denied_invokers: std::collections::HashSet<Actor>,
    granted_outside: std::collections::HashSet<String>,
    asked_first: std::collections::HashSet<String>,
}

/// The authorship rule ([`SimpleAuthority`]) with what a platform's policy would add on top,
/// stood in for: denials of a module, an action or an invoker, a grant of an outside action, and
/// a rule that asks the person first before an action.
/// Only for seeing how the runtime takes each answer; the platform's policy is
/// `tilo_action_policy`'s.
#[derive(Clone, Default)]
pub struct TestAuthority {
    authorship: SimpleAuthority,
    overrides: std::rc::Rc<std::cell::RefCell<Overrides>>,
}

impl TestAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trust(&self, module: &ModuleHash) {
        self.authorship.trust(module);
    }

    pub fn revoke(&self, module: &ModuleHash) {
        self.authorship.revoke(module);
    }

    pub fn trust_actor(&self, actor: &Actor) {
        self.authorship.trust_actor(actor);
    }

    pub fn revoke_actor(&self, actor: &Actor) {
        self.authorship.revoke_actor(actor);
    }

    pub fn deny_module(&self, module: &ModuleHash) {
        self.overrides
            .borrow_mut()
            .denied_modules
            .insert(module.clone());
    }

    pub fn deny_action(&self, action: &str) {
        self.overrides
            .borrow_mut()
            .denied_actions
            .insert(action.to_owned());
    }

    pub fn deny_invoker(&self, who: &Actor) {
        self.overrides
            .borrow_mut()
            .denied_invokers
            .insert(who.clone());
    }

    pub fn allow_invoker(&self, who: &Actor) {
        self.overrides.borrow_mut().denied_invokers.remove(who);
    }

    pub fn grant_outside(&self, action: &str) {
        self.overrides
            .borrow_mut()
            .granted_outside
            .insert(action.to_owned());
    }

    pub fn revoke_outside(&self, action: &str) {
        self.overrides.borrow_mut().granted_outside.remove(action);
    }

    /// A rule of the person's that asks them first before `action` runs, however it is called.
    pub fn ask_first(&self, action: &str) {
        self.overrides
            .borrow_mut()
            .asked_first
            .insert(action.to_owned());
    }
}

impl Authority for TestAuthority {
    fn decide(&self, invoker: &Actor, chain: &[ModuleHash], action: &ActionInfo) -> Decision {
        let overrides = self.overrides.borrow();
        if overrides.denied_actions.contains(&action.action)
            || overrides.denied_invokers.contains(invoker)
            || chain.iter().any(|m| overrides.denied_modules.contains(m))
        {
            return Decision::Deny;
        }
        if action.class == tilolang_runtime::meta::EffectClass::Outside
            && overrides.granted_outside.contains(&action.action)
            && chain.iter().all(|m| self.authorship.is_trusted(m))
        {
            return Decision::Allow;
        }
        if overrides.asked_first.contains(&action.action) {
            return Decision::Ask;
        }
        self.authorship.decide(invoker, chain, action)
    }

    fn asks_first(&self, _invoker: &Actor, action: &ActionInfo) -> bool {
        self.overrides.borrow().asked_first.contains(&action.action)
    }
}

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

/// Metadata from the JSON the tests write by hand, with the keys the compiler always emits
/// filled in.
pub fn meta(mut v: Json) -> Meta {
    fill(&mut v);
    serde_json::from_value(v).unwrap()
}

fn fill(v: &mut Json) {
    let obj = v.as_object_mut().unwrap();
    for (key, default) in [
        ("title", json!("")),
        ("module_hash", json!("")),
        ("std", json!(tilolang::stdlib::digest())),
    ] {
        obj.entry(key).or_insert(default);
    }
    for list in ["types", "actions", "views", "fns"] {
        obj.entry(list).or_insert(json!([]));
    }
    for t in obj["types"].as_array_mut().unwrap() {
        t["doc"] = t.get("doc").cloned().unwrap_or_else(|| json!(""));
        for f in t["fields"].as_array_mut().unwrap() {
            let ty = f["type"].as_str().unwrap().to_owned();
            let merge = if ty == "Text" {
                "text"
            } else if ["Collection<", "Set<", "Map<"]
                .iter()
                .any(|p| ty.starts_with(p))
            {
                "structural"
            } else {
                "register"
            };
            f["merge"] = f.get("merge").cloned().unwrap_or_else(|| json!(merge));
            f["doc"] = f.get("doc").cloned().unwrap_or_else(|| json!(""));
        }
    }
    for list in ["actions", "views"] {
        for item in obj[list].as_array_mut().unwrap() {
            for (k, d) in [
                ("doc", json!("")),
                ("params", json!([])),
                ("reaches", json!([])),
            ] {
                item[k] = item.get(k).cloned().unwrap_or(d);
            }
            if list != "views" {
                item["result"] = item.get("result").cloned().unwrap_or(Json::Null);
            }
            if list == "actions" {
                item["proposal"] = item.get("proposal").cloned().unwrap_or(Json::Null);
            }
            if list == "views" {
                item["anchors"] = item.get("anchors").cloned().unwrap_or_else(|| json!([]));
            }
        }
    }
}

pub fn me() -> Actor {
    Actor::from(tilolang_runtime::ids::MemberId::person("me"))
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

/// An action that invokes `inc` twice on its counter and returns the counter's value.
pub fn twice_action(export: &str) -> String {
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

/// The literals the counter views and actions use.
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
        .func(&twice_action("a_twice"))
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
        "views": [
            {"id": "vc", "name": "counter", "params": c, "anchors": [{"id": "p", "name": "plus"}]},
            {"id": "vmut", "name": "mutating", "params": c},
            {"id": "vspace", "name": "spacey", "params": c},
        ],
        "actions": [
            {"id": "inc", "name": "increment", "doc": "Add one to the counter.", "class": "undoable", "params": c},
            action("bad", "bad", "undoable", &c),
            action("spin", "spin", "undoable", &c),
            action("grow", "grow", "undoable", &c),
            action("forge", "forge", "undoable", &c),
            {"id": "twice", "name": "twice", "doc": "", "class": "undoable", "params": c, "result": "Int"},
        ],
    }))
}

/// The host's dispatch in these tests: every call the runtime sends on is kept, and an asked one
/// waits as the call `call-<n>` until the test answers it with [`Runtime::answer_call`], as the
/// app that holds the resource would.
#[derive(Clone, Default)]
pub struct Dispatch {
    calls: Rc<RefCell<Vec<Invocation>>>,
}

impl Dispatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The calls sent on, in the order they were made.
    pub fn calls(&self) -> Vec<Invocation> {
        self.calls.borrow().clone()
    }

    /// The calls made so far.
    pub fn len(&self) -> usize {
        self.calls.borrow().len()
    }

    /// The call a run waits on: the last one sent.
    pub fn waiting(&self) -> Invocation {
        self.calls
            .borrow()
            .last()
            .cloned()
            .expect("a call was sent")
    }

    /// The action and its arguments, in parameter order, of the call the run waits on.
    pub fn asked(&self) -> (String, Vec<Value>) {
        match self.waiting() {
            Invocation::Action { action, args, .. } => {
                (action, args.into_iter().map(|(_, value)| value).collect())
            }
            other @ Invocation::Edit { .. } => {
                panic!("the run waits on no action: {other:?}")
            }
        }
    }

    /// Run the action the run waits on, as the app that holds it would, and answer `run` with
    /// what it came to: what the dispatch does when the person allows the call.
    pub fn allow(&self, rt: &Runtime, run: &RunId) -> RunOutcome {
        let (action, args) = self.asked();
        let answer = match rt.invoke_action(&me(), (action, args)) {
            Ok(InvokeOutcome::Done(value)) => Invoked::Done(value),
            Ok(other) => panic!("the asked action did not run: {other:?}"),
            Err(error) => Invoked::Failed(error.to_string()),
        };
        rt.answer_call(run, answer).expect("the run goes on")
    }

    /// The person's yes on the system's confirm to the call the run waits on: the dispatch
    /// admits the call as answered, runs it, and answers `run` with what it came to.
    pub fn confirm(&self, rt: &Runtime, run: &RunId) -> RunOutcome {
        let (action, args) = self.asked();
        let answer = match rt.invoke_admitted(&me(), (action, args), None) {
            Ok(InvokeOutcome::Done(value)) => Invoked::Done(value),
            Ok(other) => panic!("the confirmed action did not run: {other:?}"),
            Err(error) => Invoked::Failed(error.to_string()),
        };
        rt.answer_call(run, answer).expect("the run goes on")
    }
}

/// Refuse the call the run waits on, as the dispatch does when the person declines: the run ends
/// and what it wrote is taken back.
pub fn decline(rt: &Runtime, run: &RunId) -> tilolang_runtime::RuntimeError {
    rt.answer_call(run, Invoked::Refused("declined by me".to_owned()))
        .expect_err("a declined call ends the run")
}

impl Invoker for Dispatch {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let mut calls = self.calls.borrow_mut();
        calls.push(invocation.clone());
        let n = calls.len();
        match invocation {
            Invocation::Action { asks: true, .. } => Invoked::Pending(format!("call-{n}")),
            // The tests' packages run their own actions; a call nobody is asked about is not one
            // this test host runs.
            _ => Invoked::Refused(format!("the test dispatch runs no call {n} unasked")),
        }
    }
}

/// A runtime over `store` with the counter package loaded.
pub struct World {
    pub rt: Runtime,
    pub store: AmStore,
    pub auth: TestAuthority,
    pub counter: PackageId,
    pub me: Actor,
    /// What the host sends its calls to.
    pub dispatch: Dispatch,
}

impl World {
    pub fn new() -> Self {
        Self::with(&AmStore::new(), DEFAULT_LIMITS)
    }

    pub fn with(store: &AmStore, limits: EngineLimits) -> Self {
        let auth = TestAuthority::new();
        let rt = Runtime::with_limits(store.clone(), auth.clone(), limits).unwrap();
        let dispatch = Dispatch::new();
        rt.set_foreign_invoker(dispatch.clone());
        let counter = rt.load_package(&counter_wasm(), counter_meta()).unwrap();
        Self {
            rt,
            store: store.clone(),
            auth,
            counter,
            me: me(),
            dispatch,
        }
    }

    /// A world as [`World::new`], but whose host has no dispatch: a call that needs a person's
    /// yes is refused, since nobody here can be asked.
    pub fn bare() -> Self {
        let store = AmStore::new();
        let auth = TestAuthority::new();
        let rt = Runtime::with_limits(store.clone(), auth.clone(), DEFAULT_LIMITS).unwrap();
        let counter = rt.load_package(&counter_wasm(), counter_meta()).unwrap();
        Self {
            rt,
            store,
            auth,
            counter,
            me: me(),
            dispatch: Dispatch::new(),
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
  (call $tl_coll_move (local.get $l) (call $L_fi) (local.get $it)
    (call $tl_list_get_h (call $tl_res_get_h (local.get $l) (call $L_fi)) (i64.const 0))
    (i32.const 0)))"#,
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
    let l = json!({"name": "l", "type": "Bag"});
    meta(json!({
        "app": "so.tilo.list",
        "types": [
            {"id": "tl", "name": "Bag",
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
        "views": [{"id": "vl", "name": "bag", "params": [l]}],
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

/// A module by someone else: a view whose button invokes the counter app's `inc`, and actions
/// that invoke it, one of which calls a model first.
pub fn stranger_wasm() -> Vec<u8> {
    counter_lits(Wat::new())
        .lit("kind", "classify")
        .lit("input", "hello")
        .func(&counter_view("v_vs"))
        .func(&twice_action("a_ask"))
        .func(
            r#"(func (export "a_boom") (param $c i32) (result i64)
  (local $args i32) (local $a i32)
  (local.set $args (call $tl_list_new))
  (call $tl_list_push_h (local.get $args) (local.get $c))
  (local.set $a (call $tl_act_ref (call $L_inc) (local.get $args)))
  (drop (call $tl_invoke (local.get $a)))
  (drop (call $tl_invoke (local.get $a)))
  unreachable)"#,
        )
        .func(
            r#"(func (export "a_m") (param $c i32) (result i32)
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
    let c = json!([{"name": "c", "type": "so.tilo.counter.Counter"}]);
    meta(json!({
        "app": "x.stranger",
        "views": [{"id": "vs", "name": "strange", "params": c}],
        "actions": [
            {"id": "ask", "name": "ask", "doc": "", "class": "undoable", "params": c, "result": "Int"},
            {"id": "boom", "name": "boom", "doc": "", "class": "undoable", "params": c, "result": "Int"},
            {"id": "m", "name": "m", "doc": "", "class": "undoable", "params": c, "result": "Text"},
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

/// The directory of the app `apps/tl/<name>`.
fn app_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../apps/tl")
        .join(name)
}

/// The app `apps/tl/<name>` and the packages it uses, built the way a real build builds them:
/// each from every file of its directory with its committed ids, in dependency order, against
/// the platform's packages. The packages it uses come first and the app last. Each directory is
/// compiled once in a test process.
pub fn built(name: &str) -> Vec<App> {
    static BUILT: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, App>>,
    > = std::sync::OnceLock::new();
    let platform =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let dirs = tilolang::source::dependency_order_with(
        &app_dir(name),
        &[],
        &tilolang::source::apps_of(&platform),
    )
    .unwrap_or_else(|e| panic!("apps/tl/{name}: {e}"));
    let cache = BUILT.get_or_init(Default::default);
    let mut out: Vec<App> = Vec::new();
    for dir in dirs {
        let dir = dir.canonicalize().unwrap_or(dir);
        let app = cached(cache, &dir, || {
            let source = tilolang::PackageSource::read_dir(&dir).unwrap();
            let ids = tilolang::source::read_ids(&dir).unwrap();
            let mut imports = platform.clone();
            imports.extend(out.iter().map(|used| used.meta().clone()));
            let compiled = tilolang::compile_package_source_with(
                &source,
                &ids,
                &imports,
                &mut tilolang::SeededEntropy(0),
            )
            .unwrap_or_else(|d| panic!("{}: {d:#?}", dir.display()));
            assert_eq!(
                compiled.ids.to_text(),
                ids.to_text(),
                "{}/app.ids is out of date: run `tilolang build` on it",
                dir.display()
            );
            let (_, uses) = source.header();
            App {
                compiled,
                platform: tilolang_tilo::platform_apps::platform_packages_of(
                    uses.iter().map(String::as_str),
                )
                .expect("the platform's packages read"),
            }
        });
        out.push(app);
    }
    out
}

/// `dir`'s app from `cache`, built with `build` when it holds none. The cache stays held while
/// the build runs, so a directory is compiled once even when two tests want it at once.
fn cached(
    cache: &std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, App>>,
    dir: &std::path::Path,
    build: impl FnOnce() -> App,
) -> App {
    let mut held = cache.lock().unwrap();
    if let Some(app) = held.get(dir) {
        return app.clone();
    }
    let app = build();
    held.insert(dir.to_owned(), app.clone());
    app
}

/// The app `apps/tl/<name>` as [`built`] builds it.
pub fn app(name: &str) -> App {
    built(name).pop().expect("the app is built last")
}

/// The packages the app `apps/tl/<name>` uses, built as [`built`] builds them and loaded into a
/// runtime: what a package compiled beside that app compiles against.
pub struct Uses {
    pub apps: Vec<App>,
}

impl Uses {
    /// Build and load each into `rt`, handing `trust` what was loaded (an installed package is
    /// code the platform trusts).
    pub fn load(rt: &Runtime, name: &str, trust: impl Fn(&PackageId)) -> Self {
        let mut apps = built(name);
        apps.pop();
        for used in &apps {
            trust(&used.load(rt));
        }
        Self { apps }
    }

    /// What a package beside the app compiles against.
    pub fn metas(&self) -> Vec<tilolang::PackageMeta> {
        self.apps.iter().map(|used| used.meta().clone()).collect()
    }

    /// The used package whose app id is `app`.
    pub fn app(&self, app: &str) -> &App {
        self.apps
            .iter()
            .find(|used| used.meta().app == app)
            .unwrap_or_else(|| panic!("no package {app} is used"))
    }
}

/// The platform's packages `src` names with `use` that `imports` leaves out: what a real build
/// compiles a package against besides the packages it is given.
pub fn platform_named(src: &str, imports: &[tilolang::PackageMeta]) -> Vec<tilolang::PackageMeta> {
    tilolang_tilo::platform_apps::named_platform_packages(src)
        .expect("the platform's packages read")
        .into_iter()
        .filter(|package| !imports.iter().any(|given| given.app == package.app))
        .collect()
}

/// A package compiled from tilolang source and loaded into a runtime.
#[derive(Clone)]
pub struct App {
    pub compiled: tilolang::Compiled,
    /// The platform's packages it was compiled against, which loading it describes beside it.
    pub platform: Vec<tilolang::PackageMeta>,
}

impl App {
    /// `src` compiled against `imports` and the platform's packages it names.
    pub fn compile(src: &str, imports: &[tilolang::PackageMeta]) -> Self {
        let platform = platform_named(src, imports);
        let mut all = imports.to_vec();
        all.extend(platform.iter().cloned());
        let compiled = tilolang::compile_package(src, &tilolang::IdsLock::new(), &all)
            .unwrap_or_else(|d| panic!("{d:#?}"));
        Self { compiled, platform }
    }

    /// Load it into `rt` as a host loads a package: its module, then the description of each
    /// platform package it names.
    pub fn load(&self, rt: &Runtime) -> PackageId {
        let id = rt.load_compiled(&self.compiled).unwrap();
        for meta in &self.platform {
            rt.load_foreign(meta.clone())
                .unwrap_or_else(|e| panic!("{}: {e}", meta.app));
        }
        id
    }

    pub fn meta(&self) -> &tilolang::PackageMeta {
        &self.compiled.meta
    }

    pub fn action(&self, name: &str) -> String {
        let a = self.meta().action_named(name);
        a.unwrap_or_else(|| panic!("no action {name}")).id.clone()
    }

    pub fn view(&self, name: &str) -> String {
        let v = self.meta().views.iter().find(|v| v.name == name);
        v.unwrap_or_else(|| panic!("no view {name}")).id.clone()
    }

    pub fn fn_(&self, name: &str) -> String {
        let f = self.meta().fns.iter().find(|f| f.name == name);
        f.unwrap_or_else(|| panic!("no fn {name}")).id.clone()
    }

    /// Where `view` puts the control written `as name`: its own anchor, or one a view it calls
    /// declares, including the views it expands (`templates`), as their nodes stand in its frame.
    pub fn anchor(&self, view: &str, name: &str) -> String {
        let meta = self.meta();
        let views = || meta.views.iter().chain(meta.template_views.iter());
        let start = views()
            .find(|v| v.name == view)
            .unwrap_or_else(|| panic!("no view {view}"));
        let mut seen = std::collections::HashSet::new();
        let mut look = vec![start];
        let mut searched: Vec<&str> = Vec::new();
        while let Some(v) = look.pop() {
            if !seen.insert(v.id.as_str()) {
                continue;
            }
            searched.push(v.name.as_str());
            if let Some(a) = v.anchors.iter().find(|a| a.name == name) {
                return a.id.clone();
            }
            for id in &v.calls {
                if let Some(called) = views().find(|c| c.id == *id) {
                    look.push(called);
                }
            }
        }
        panic!(
            "no anchor `{name}` in view `{view}` or the views it calls ({})",
            searched.join(", ")
        );
    }

    /// The arguments of `action` with the parameters `given` names set and the rest left to
    /// their defaults, as an invocation carries them: what a caller says it means, so a
    /// parameter the app adds or takes away changes no caller.
    pub fn args(&self, rt: &Runtime, action: &str, given: &[(&str, Value)]) -> Vec<Value> {
        let meta = self
            .meta()
            .actions
            .iter()
            .find(|a| a.name == action)
            .unwrap_or_else(|| panic!("no action {action}"));
        let mut args: Vec<Option<Value>> = meta.params.iter().map(|_| None).collect();
        for (name, value) in given {
            let at = meta
                .params
                .iter()
                .position(|p| p.name == *name)
                .unwrap_or_else(|| panic!("{action} has no parameter {name}"));
            *args
                .get_mut(at)
                .unwrap_or_else(|| panic!("{action} has no parameter {name}")) =
                Some(value.clone());
        }
        rt.complete_args(&meta.id, args)
            .unwrap_or_else(|e| panic!("{action}: {e}"))
    }
}

/// An indented dump of a tree, for assertions on its shape.
pub fn dump(node: &tilolang_runtime::UiNode) -> String {
    fn go(n: &tilolang_runtime::UiNode, depth: usize, out: &mut String) {
        use std::fmt::Write as _;
        let props: Vec<String> = n.props.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
        let events: Vec<&String> = n.events.keys().collect();
        let pad = "  ".repeat(depth);
        writeln!(
            out,
            "{pad}{} [{}] {} {events:?}",
            n.kind,
            n.key,
            props.join(" ")
        )
        .unwrap();
        for c in &n.children {
            go(c, depth.saturating_add(1), out);
        }
    }
    let mut out = String::new();
    go(node, 0, &mut out);
    out
}

/// The nodes of a tree of the given kind, in document order.
pub fn of_kind<'a>(
    node: &'a tilolang_runtime::UiNode,
    kind: &str,
) -> Vec<&'a tilolang_runtime::UiNode> {
    node.of_kind(kind)
}

/// The resource an action returned.
pub fn returned_ref(out: tilolang_runtime::InvokeOutcome) -> tilolang_runtime::ResourceRef {
    match out {
        tilolang_runtime::InvokeOutcome::Done(Value::Ref(r)) => {
            std::rc::Rc::unwrap_or_clone(r.target)
        }
        other => panic!("expected a returned resource, got {other:?}"),
    }
}

/// The text of a node's property, which must be text.
pub fn text_prop(node: &tilolang_runtime::UiNode, name: &str) -> String {
    match node.props.get(name) {
        Some(Value::Text(t)) => t.clone(),
        other => panic!("property {name} of {} is {other:?}", node.kind),
    }
}

/// The person `id`.
#[allow(dead_code, reason = "not every test target names people")]
pub fn person(id: &str) -> Actor {
    tilolang_runtime::ids::MemberId::person(id).into()
}

/// The agent `id`.
#[allow(dead_code, reason = "not every test target names agents")]
pub fn agent(id: &str) -> Actor {
    tilolang_runtime::ids::MemberId::agent(id, "ada").into()
}

/// The notices a run raised, kept for a test to read: what the desktop's notifications would
/// show, in the order the runs raised them.
#[derive(Clone, Default)]
pub struct Raised(pub std::rc::Rc<std::cell::RefCell<Vec<tilolang_runtime::notify::Notice>>>);

impl tilolang_runtime::notify::Notices for Raised {
    fn raised(&self, notice: &tilolang_runtime::notify::Notice) {
        self.0.borrow_mut().push(notice.clone());
    }
}

impl Raised {
    /// The notices raised so far.
    #[must_use]
    pub fn notices(&self) -> Vec<tilolang_runtime::notify::Notice> {
        self.0.borrow().clone()
    }
}

/// A record an app's origin writes, as its poll writes an answer from a service: a new item of
/// `ty` in the collection `field` of `owner`, its `fields` set, all in one change made by
/// `origin`. The item is answered. The app reads it exactly as it reads what its outside service
/// brings, so a test seeds the catalogue, the video site's answer or a model's reading the way
/// they arrive rather than by writing the app's own record.
pub fn seed_origin(
    rt: &Runtime,
    origin: &str,
    owner: &tilolang_runtime::ResourceRef,
    field: &str,
    ty: &str,
    fields: Vec<(&str, Value)>,
) -> tilolang_runtime::ResourceRef {
    let write = tilolang_runtime::origin::OriginWrite::Insert {
        owner: owner.clone(),
        field: field.to_owned(),
        ty: ty.to_owned(),
        fields: fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    };
    let made = rt
        .write_origin(origin, None, |change| change.write(vec![write]))
        .unwrap_or_else(|why| panic!("seeding a record for {origin}: {why}"));
    match made.into_iter().next() {
        Some(Some(item)) => item,
        other => panic!("an origin's insert of {ty} into {field} made no record: {other:?}"),
    }
}

/// The id of the type `name` in `app`'s package: what an origin names in an insert, since the
/// store knows a type by its id and not by the name the package writes.
pub fn type_id(app: &App, name: &str) -> String {
    app.meta()
        .types
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("{name} is no type of this package"))
        .id
        .clone()
}

/// The id of the field `name` of the type `ty` in `app`'s package: a collection insert names
/// the collection by its id, since the store keys a document's parts by them.
pub fn field_id(app: &App, ty: &str, name: &str) -> String {
    let declared = app
        .meta()
        .types
        .iter()
        .find(|t| t.name == ty)
        .unwrap_or_else(|| panic!("{ty} is no type of this package"));
    declared
        .fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{ty} has no field {name}"))
        .id
        .clone()
}

/// The jobs a test's device runs, each by the latest record it told, as the device keeps them:
/// what `engine.job` answers.
#[derive(Clone, Default)]
pub struct DeviceJobs {
    records: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Json>>>,
    started: Rc<std::cell::Cell<u64>>,
}

impl DeviceJobs {
    /// Start `op` of `engine` for `app` as a job, whose moves are kept here: answered with its
    /// path.
    pub fn start(
        &self,
        engine: &dyn tilolang_tilo::engine::Provider,
        app: &str,
        op: &str,
        input: &str,
    ) -> Result<String, String> {
        let (job, moved) = self.begin(app, op);
        engine.start(&job, op, input, moved)?;
        Ok(job)
    }

    /// A job of `op` for `app` about to start: its path, and the sink its engine tells its moves
    /// to.
    pub fn begin(&self, app: &str, op: &str) -> (String, tilolang_tilo::engine::JobSink) {
        self.started.set(self.started.get().wrapping_add(1));
        let job = format!("devices/test/jobs/{}", self.started.get());
        let (records, named, app, doing) = (
            std::sync::Arc::clone(&self.records),
            job.clone(),
            app.to_owned(),
            op.to_owned(),
        );
        let moved = tilolang_tilo::engine::JobSink::new(move |state, detail| {
            let record = tilolang_tilo::engine::record(&named, &app, &doing, state, &detail);
            records.lock().unwrap().insert(named.clone(), record);
        });
        (job, moved)
    }

    /// The job `job` as its last move left it.
    pub fn job(&self, job: &str) -> Option<Json> {
        self.records.lock().unwrap().get(job).cloned()
    }
}

/// The artifact the device daemon would compile for `wasm`, handed to this process: from its
/// first load the module runs compiled instead of on the interpreter. What a device's daemon
/// does at install, done here by the one compiler so the compiled tier is exercised.
pub fn adopt_compiled(wasm: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let cache = tilo_wasm_aot::Cache::at(dir.path()).unwrap();
    let entry = cache.ensure(wasm).unwrap();
    tilolang_runtime::engine::adopt_precompiled(wasm, &entry).unwrap();
}

/// Stage a workspace of `members` for `rt`, as a host with no workspace behind it holds one: all
/// of them in the workspace's members' resource, a plain record, and with their roles in its
/// policy, which decides by `reads` who reads what (`None`: everyone reads everything). Whether
/// the members' resource moved.
pub fn stage_members(
    rt: &Runtime,
    members: Vec<tilolang_runtime::members::Member>,
    reads: Option<Rc<tilolang_runtime::members::Reads>>,
) -> bool {
    let root = tilolang_tilo::platform_apps::workspace_members();
    rt.set_members_root(&root);
    let people: Vec<Json> = members
        .iter()
        .map(|member| tilolang_runtime::value::to_json(&Value::Actor(member.who())).unwrap())
        .collect();
    let moved = rt
        .set_reading(&root, &json!({ "people": people }).to_string())
        .unwrap();
    rt.set_member_policy(members, reads);
    moved
}
