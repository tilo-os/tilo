use crate::common::{todo_meta, validate};
use tilolang::BlockKind;

fn block(src: &str) -> Result<tilolang::CompiledBlock, Vec<tilolang::Diagnostic>> {
    tilolang::compile_block(src, &[todo_meta()])
}

fn exports(wasm: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for p in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::ExportSection(r) = p.expect("parses") {
            out.extend(r.into_iter().map(|e| e.expect("export").name.to_string()));
        }
    }
    out
}

#[test]
fn an_action_application_is_an_action_block() {
    let b = block("todo.add(@todo.TodoList:01J8ZK4T, \"Milk\")").expect("compiles");
    validate(&b.wasm);
    assert_eq!(b.kind, BlockKind::Action);
    assert_eq!(b.value_type, "Action");
    assert!(exports(&b.wasm).contains(&"b_main".to_string()));
}

#[test]
fn a_block_that_suggests_an_action_proposes_it() {
    let b = block("suggest todo.add(@todo.TodoList:01J8ZK4T, \"Milk\")").expect("compiles");
    validate(&b.wasm);
    assert_eq!(b.kind, BlockKind::Action);
    assert!(b.suggested, "the block is a proposal");
    assert_eq!(b.value_type, "Action");
    assert!(exports(&b.wasm).contains(&"b_main".to_string()));
}

#[test]
fn an_action_the_block_applies_is_no_proposal() {
    let b = block("todo.add(@todo.TodoList:01J8ZK4T, \"Milk\")").expect("compiles");
    assert!(!b.suggested);
}

#[test]
fn suggest_is_refused_where_no_proposal_may_be_made() {
    // A view runs as it is drawn: it cannot propose for the person looking.
    let err = block(
        "stack {\n    button(\"Add\", on: suggest todo.add(@todo.TodoList:01J8ZK4T, \"x\"))\n}",
    )
    .expect_err("fails");
    assert_eq!(err[0].code, "E044", "{}", err[0].message);
    // A block proposes its whole value, and only when the value is the proposal itself.
    let err = block("if true { suggest todo.add(@todo.TodoList:01J8ZK4T, \"x\") } else { 1 }")
        .expect_err("fails");
    assert_eq!(err[0].code, "E044", "{}", err[0].message);
    // Only an action is proposed.
    let err = block("suggest 1").expect_err("fails");
    assert_eq!(err[0].code, "E044", "{}", err[0].message);
    let err = block("suggest todo.todo_list(@todo.TodoList:01J8ZK4T)").expect_err("fails");
    assert_eq!(err[0].code, "E044", "{}", err[0].message);
}

#[test]
fn view_code_is_a_view_block() {
    let b = block("stack {\n    text(\"Good morning\")\n    button(\"Add\", on: todo.add(@todo.TodoList:01J8ZK4T, \"x\"))\n}").expect("compiles");
    validate(&b.wasm);
    assert_eq!(b.kind, BlockKind::View);
}

#[test]
fn a_foreign_view_is_applied_by_the_host() {
    let b = block("todo.todo_list(@todo.TodoList:01J8ZK4T)").expect("compiles");
    validate(&b.wasm);
    assert_eq!(b.kind, BlockKind::View);
}

#[test]
fn data_blocks_have_a_value_type() {
    let b = block("@todo.TodoList:01J8ZK4T").expect("compiles");
    validate(&b.wasm);
    assert_eq!(b.kind, BlockKind::Data);
    assert_eq!(b.value_type, "so.tilo.todo.TodoList");
    let n = block("1 + 2").expect("compiles");
    validate(&n.wasm);
    assert_eq!(n.value_type, "Int");
}

#[test]
fn blocks_cannot_mutate_or_use_unknown_names() {
    let err = block("todo.nothing(1)").expect_err("fails");
    assert_eq!(err[0].code, "E014");
    let err = block("stack {").expect_err("fails");
    assert_eq!(err[0].code, "E003");
}

#[test]
fn the_block_module_is_authored_as_an_agent_block() {
    let b = tilolang::compile_block_for("run1", "1", &[]).expect("compiles");
    assert_eq!(b.meta.app, "agent.block.run1");
}

#[test]
fn a_reference_to_an_item_is_written_with_its_document_and_id() {
    let b = block("todo.toggle(@todo.Todo:d1-1/i1-5)").expect("compiles");
    validate(&b.wasm);
    assert_eq!(b.kind, BlockKind::Action);
    assert!(
        b.wasm.windows(9).any(|w| w == "d1-1/i1-5".as_bytes()),
        "the whole id reaches the module's strings"
    );
}

#[test]
fn a_reference_written_as_the_path_to_an_item_still_compiles() {
    let b = block("todo.toggle(@todo.Todo:d1-1/f3a:i1-5)").expect("compiles");
    validate(&b.wasm);
    assert!(
        b.wasm.windows(13).any(|w| w == "d1-1/f3a:i1-5".as_bytes()),
        "the whole id reaches the module's strings, which the runtime reads as its last item"
    );
}
