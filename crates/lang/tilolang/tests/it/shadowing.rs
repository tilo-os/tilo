//! A package's own declaration takes the name it declares, everywhere in the package, even where
//! it is a name the language provides a type for (README, "Type names"). What the language
//! provides of that name stays reachable as `std.`'s.

use crate::common::validate;
use tilolang::{IdsLock, compile_package};

fn compile(src: &str) -> tilolang::Compiled {
    let c = compile_package(src, &IdsLock::new(), &[]).unwrap_or_else(|d| panic!("{src}\n{d:#?}"));
    validate(&c.wasm);
    c
}

/// A package whose `File` is its own type, and whose one field of `Text` is the language's.
const SHADOWED: &str = r#"app t.x "X"
type File {
    title: Text
    text_of_it: std.Text
}

type Board {
    files: Collection<File>
    cover: Option<File> = .none
}

action retitle(b: Board, f: File) {
    f.title = "Untitled"
    f.text_of_it = ""
    b.cover = f
}

fn titled(f: File) -> Text {
    f.title
}

view board(b: Board) {
    stack {
        for f in b.files {
            text("{titled(f)}")
        }
        text("{b.files.len()}")
        text(b.cover?.title ?? "")
    }
}
"#;

fn field_ty(c: &tilolang::Compiled, ty: &str, field: &str) -> String {
    c.meta
        .types
        .iter()
        .find(|t| t.name == ty)
        .unwrap_or_else(|| panic!("no type {ty}"))
        .fields
        .iter()
        .find(|f| f.name == field)
        .unwrap_or_else(|| panic!("no field {ty}.{field}"))
        .ty
        .clone()
}

/// Every use of `File` in the package is the package's own type: a field, a parameter, a collection's item,
/// an option, a view's loop, and the metadata the host reads.
#[test]
fn a_declared_type_shadows_the_language_type_of_its_name() {
    let c = compile(SHADOWED);
    assert_eq!(field_ty(&c, "Board", "files"), "Collection<t.x.File>");
    assert_eq!(field_ty(&c, "Board", "cover"), "Option<t.x.File>");
    // The language's `Text` is a field of it, and the language's `File` names nothing here.
    assert_eq!(field_ty(&c, "File", "title"), "Text");
}

/// The language's own type of a shadowed name stays reachable as `std.`'s, so nothing the language provides
/// is out of a package's reach.
#[test]
fn the_language_type_is_reachable_through_std() {
    let c = compile(SHADOWED);
    assert_eq!(field_ty(&c, "File", "text_of_it"), "Text");
    // A package that declares `Text` writes the language's one as `std.Text`.
    let c = compile(
        "app t.x \"X\"\ntype Text {\n    s: std.Text\n}\ntype T {\n    t: Text\n    u: std.Text\n}\n",
    );
    assert_eq!(field_ty(&c, "T", "t"), "t.x.Text");
    assert_eq!(field_ty(&c, "T", "u"), "Text");
}

/// A name a package declares out of the language's own list is no error at all: the declaration is the type
/// from then on, in a field, a parameter, a view, an enum or an alias alike.
#[test]
fn a_declaration_of_a_language_type_name_is_no_diagnostic() {
    for body in [
        "type Text {\n    s: std.Text\n}",
        "type Date {\n    d: std.Date\n}",
        "type Action {\n    x: std.Text\n}",
        "enum Response {\n    ok\n}",
        "type View = std.View",
        "type T {\n    k: File\n}\nenum File {\n    draft\n    final\n}",
        "type T {\n    s: Text\n}\ntype Text = std.Identifier",
    ] {
        compile(&format!("app t.x \"X\"\n{body}\n"));
    }
}

/// Declaring one of the names the grammar writes a type with is refused where it is written, by one message
/// that says what to do instead.
#[test]
fn a_name_the_grammar_writes_a_type_with_is_refused() {
    for decl in [
        "type List {\n    items: Int\n}",
        "enum Option {\n    a\n}",
        "type Collection = Int",
        "type Result = std.Text",
    ] {
        let err = compile_package(&format!("app t.x \"X\"\n{decl}\n"), &IdsLock::new(), &[])
            .expect_err(&format!("`{decl}` declares a type the grammar writes"));
        let said: Vec<&str> = err.iter().map(|d| d.message.as_str()).collect();
        assert!(
            said.iter()
                .any(|m| m.contains("name yours differently") && m.contains("writes a type")),
            "{decl}: {said:?}"
        );
    }
}

/// A name the language does not provide is still unknown, and the message lists what there is.
#[test]
fn an_unknown_name_is_still_unknown() {
    let src = "app t.x \"X\"\ntype T {\n    x: Filez\n}\n";
    let err = compile_package(src, &IdsLock::new(), &[]).expect_err("no such type");
    let d = err
        .iter()
        .find(|d| d.code == "E013")
        .unwrap_or_else(|| panic!("{err:#?}"));
    assert!(d.message.contains("unknown type `Filez`"), "{}", d.message);
    assert!(d.message.contains("File, Response"), "{}", d.message);
    assert_eq!(d.span.start, src.find("Filez").unwrap());
}
