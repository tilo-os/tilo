//! The agent library (`tilo.agent`): the words a session has by default, the same words a
//! package calls after `use`, and the errors a package without the `use` gets.

use crate::common::{codes_with, compile_with, todo_meta, validate};

/// A session's code has the library's words without a `use`: `grep`, `find`, `read`,
/// `describe`, `print` and `me`.
#[test]
fn a_session_has_the_words_in_scope() {
    let visible = [todo_meta()];
    let program = "grep(/Tide\\b/, in: todo, not: [.quote], changed: -14d, mentions: me)\n\
                   print(me)\n\
                   print(find(\"tide\"))\n\
                   read(find(\"tide\").single())\n\
                   describe(todo)";
    let compiled =
        tilolang::compile_program(program, &visible, &[]).unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
}

/// The words chaining on what they answer are the methods of any list, not words of their own.
#[test]
fn a_session_chains_the_library_words() {
    let visible = [todo_meta()];
    let compiled = tilolang::compile_program(
        "let hits = grep(/(\\w+)/, in: todo)\n\
         print(hits.count_by(.doc))\n\
         print(hits.take(3))\n\
         print(hits.of(todo.Todo).take(2))\n\
         hits.each(h -> print(h.line))\n\
         print(hits.single().line)",
        &visible,
        &[],
    )
    .unwrap_or_else(|d| panic!("{d:#?}"));
    validate(&compiled.wasm);
}

/// A package without the `use` has no words: they are one library's, not the language's.
#[test]
fn a_package_without_the_use_has_no_words() {
    let visible = [todo_meta()];
    assert_eq!(
        codes_with(
            "use so.tilo.todo as todo\nfn look() -> Text { return grep(/Tide/, in: todo) }",
            &visible,
        ),
        ["E014"]
    );
    assert_eq!(
        codes_with("fn who() -> Principal { return me }", &visible,),
        ["E014"]
    );
}

/// A package that wants the words `use`s the library as any other: `agent.grep(...)`,
/// `agent.me`, and the package's `uses` naming it.
#[test]
fn a_package_uses_the_library_explicitly() {
    let visible = [todo_meta()];
    let src = "app t.peek \"Peek\"\n\
               use so.tilo.todo as todo\n\
               use tilo.agent as agent\n\
               action peek(n: todo.Todo) -> Text {\n\
                   let hits = agent.grep(/Tide/, in: n, not: [.quote])\n\
                   agent.print(hits, fields: [.text])\n\
                   return agent.describe(n)\n\
               }\n\
               action mine() -> Principal {\n\
                   return agent.me\n\
               }\n";
    let compiled = compile_with(src, &visible).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(
        compiled.meta.uses,
        ["so.tilo.todo", "tilo.agent", "tilo.std"]
    );
}

/// The kinds of block `not:` leaves out are the library's, and a wrong one is answered with
/// them.
#[test]
fn the_kinds_of_block_come_from_the_library() {
    let errs = tilolang::compile_program("grep(/x/, not: .qoute)", &[], &[])
        .err()
        .unwrap_or_default();
    let message = errs.first().map(|d| d.message.clone()).unwrap_or_default();
    assert!(
        message.contains(".paragraph, .heading"),
        "the error lists the kinds: {message}"
    );
}
