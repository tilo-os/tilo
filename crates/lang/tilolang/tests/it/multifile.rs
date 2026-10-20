//! Packages of several files: one module, the `app` header in exactly one file, any file order,
//! diagnostics that name their file, and the directory tools (`fmt`, imports among siblings).

use std::path::{Path, PathBuf};

use tilolang::cli::format_package;
use tilolang::source::{BuildError, Stamp, compile_dirs, dependency_order};
use tilolang::{Compiled, Diagnostic, IdsLock, PackageSource, SeededEntropy};

use crate::common::fixture;

const HEAD: &str = "app so.tilo.counter \"Counter\"\n\n/// A counter that several people can bump at once.\ntype Counter {\n    /// The current value.\n    value: Int merge counter\n}\n";
const ACTIONS: &str =
    "/// Add one to the counter.\naction increment(c: Counter) {\n    c.value += 1\n}\n";
const VIEWS: &str = "view counter(c: Counter) {\n    column {\n        text(c.value)\n        button(\"+1\", on: increment(c)) #plus\n    }\n}\n";

fn files(list: &[(&str, &str)]) -> PackageSource {
    PackageSource::new(
        list.iter()
            .map(|(p, t)| ((*p).to_string(), (*t).to_string())),
    )
}

fn compile(source: &PackageSource) -> Result<Compiled, Vec<Diagnostic>> {
    tilolang::compile_package_source_with(source, &IdsLock::new(), &[], &mut SeededEntropy(7))
}

fn split_counter() -> PackageSource {
    files(&[("a.tl", HEAD), ("b.tl", ACTIONS), ("c.tl", VIEWS)])
}

#[test]
fn a_package_split_over_files_is_the_same_module_as_one_file() {
    let whole = tilolang::compile_package_with(
        &fixture("counter.tl"),
        &IdsLock::new(),
        &[],
        &mut SeededEntropy(7),
    )
    .unwrap();
    let split = compile(&split_counter()).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(split.meta, whole.meta);
    assert_eq!(split.wasm, whole.wasm);
    assert_eq!(split.ids.to_text(), whole.ids.to_text());
}

#[test]
fn the_order_files_are_given_in_does_not_matter() {
    let given = files(&[("c.tl", VIEWS), ("a.tl", HEAD), ("b.tl", ACTIONS)]);
    assert_eq!(
        compile(&given).unwrap().wasm,
        compile(&split_counter()).unwrap().wasm
    );
}

#[test]
fn a_file_uses_what_a_later_file_declares() {
    // The view and action come before the type they use, in file order.
    let source = files(&[
        ("a.tl", "app so.tilo.counter \"Counter\"\n"),
        ("b.tl", VIEWS),
        ("c.tl", ACTIONS),
        ("d.tl", "type Counter {\n    value: Int merge counter\n}\n"),
    ]);
    let compiled = compile(&source).unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(compiled.meta.views.len(), 1);
}

#[test]
fn a_package_without_the_app_header_names_the_fix() {
    let diags = compile(&files(&[("a.tl", ACTIONS), ("b.tl", VIEWS)])).unwrap_err();
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0].code, "E010");
    assert_eq!(diags[0].file.as_deref(), Some("a.tl"));
    assert!(
        diags[0].message.contains("app <app id>"),
        "{}",
        diags[0].message
    );
}

#[test]
fn a_second_app_header_is_an_error_in_its_own_file() {
    let source = files(&[
        ("a.tl", HEAD),
        (
            "b.tl",
            "app so.tilo.other \"Other\"\n\n/// Add one.\naction increment(c: Counter) {\n    c.value += 1\n}\n",
        ),
    ]);
    let diags = compile(&source).unwrap_err();
    assert_eq!(diags.len(), 1, "{diags:#?}");
    let d = &diags[0];
    assert_eq!(d.code, "E017");
    assert_eq!(d.file.as_deref(), Some("b.tl"));
    assert_eq!((d.line, d.col), (1, 1));
    assert!(d.message.contains("`a.tl`"), "{}", d.message);
}

#[test]
fn the_app_header_comes_first_in_its_file() {
    let source = files(&[(
        "a.tl",
        &format!("{ACTIONS}\napp so.tilo.counter \"Counter\"\n"),
    )]);
    let codes: Vec<String> = compile(&source)
        .unwrap_err()
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert!(codes.contains(&"E017".to_string()), "{codes:?}");
}

#[test]
fn diagnostics_carry_the_file_and_the_line_within_it() {
    let broken = "view counter(c: Counter) {\n    column {\n        text(c.missing)\n    }\n}\n";
    let source = files(&[("a.tl", HEAD), ("b.tl", ACTIONS), ("c.tl", broken)]);
    let diags = compile(&source).unwrap_err();
    let d = diags.first().unwrap();
    assert_eq!(d.file.as_deref(), Some("c.tl"));
    assert_eq!(d.line, 3);
    assert_eq!(broken.get(d.span.start..d.span.end), Some("missing"));
    assert!(
        d.render("ignored").starts_with("c.tl:3:"),
        "{}",
        d.render("ignored")
    );
    assert_eq!(d.to_json()["file"], "c.tl");
}

#[test]
fn a_declaration_repeated_in_another_file_is_reported_there() {
    let again = "type Counter {\n    value: Int\n}\n";
    let diags = compile(&files(&[("a.tl", HEAD), ("b.tl", again)])).unwrap_err();
    assert_eq!(diags[0].code, "E011");
    assert_eq!(diags[0].file.as_deref(), Some("b.tl"));
    assert_eq!(diags[0].line, 1);
}

#[test]
fn syntax_errors_in_several_files_are_all_reported() {
    let diags = compile(&files(&[
        ("a.tl", HEAD),
        ("b.tl", "action (c: Counter) {}\n"),
        ("c.tl", "view v( {\n}\n"),
    ]))
    .unwrap_err();
    let in_files: Vec<_> = diags.iter().filter_map(|d| d.file.as_deref()).collect();
    assert!(
        in_files.contains(&"b.tl") && in_files.contains(&"c.tl"),
        "{diags:#?}"
    );
}

#[test]
fn the_single_text_api_reports_no_file() {
    let diags = tilolang::check(
        "app t.x \"X\"\nfn f() -> Int {\n    return y\n}\n",
        &IdsLock::new(),
        &[],
    );
    assert!(!diags.is_empty());
    assert!(diags.iter().all(|d| d.file.is_none()));
    assert!(
        tilolang::diagnostics_json(&diags)
            .find("\"file\"")
            .is_none()
    );
}

#[test]
fn check_package_agrees_with_compile() {
    assert!(tilolang::check_package(&split_counter(), &IdsLock::new(), &[]).is_empty());
}

// ---- fmt ------------------------------------------------------------------------------------

#[test]
fn fmt_prints_each_file_on_its_own_and_keeps_the_header_in_its_file() {
    let messy = files(&[
        (
            "a.tl",
            "app so.tilo.counter   \"Counter\"\ntype Counter { value: Int merge counter }\n",
        ),
        (
            "b.tl",
            "\n\naction increment(c: Counter) { c.value += 1 }\naction decrement(c: Counter) { c.value -= 1 }\n",
        ),
    ]);
    let out = format_package(&messy).unwrap();
    assert!(out.with_comments.is_empty());
    let a = &out.files.iter().find(|f| f.0 == "a.tl").unwrap().2;
    let b = &out.files.iter().find(|f| f.0 == "b.tl").unwrap().2;
    assert!(
        a.starts_with("app so.tilo.counter \"Counter\"\n\ntype Counter {\n"),
        "{a}"
    );
    assert!(b.starts_with("action increment(c: Counter) {\n"), "{b}");
    assert!(b.contains("}\n\naction decrement"), "{b}");
    assert!(!b.contains("app "), "{b}");
    // Canonical text formats to itself.
    let again = format_package(&files(&[("a.tl", a), ("b.tl", b)])).unwrap();
    for (_, old, new) in &again.files {
        assert_eq!(old, new);
    }
}

#[test]
fn fmt_leaves_a_file_with_plain_comments_alone_and_names_it() {
    let out = format_package(&files(&[
        ("a.tl", HEAD),
        ("b.tl", "/// Add one.\naction increment(c: Counter) {\n    // why: counters merge\n    c.value += 1\n}\n"),
    ]))
    .unwrap();
    assert_eq!(out.with_comments, vec![("b.tl".to_string(), 3)]);
    assert!(out.files.iter().all(|f| f.0 != "b.tl"));
}

#[test]
fn the_example_packages_are_in_canonical_form() {
    for dir in example_dirs() {
        let source = PackageSource::read_dir(&dir).unwrap();
        let out = format_package(&source).unwrap_or_else(|d| panic!("{}: {d:#?}", dir.display()));
        for (path, old, new) in &out.files {
            assert_eq!(old, new, "{path} is not in canonical form");
        }
    }
}

// ---- package directories --------------------------------------------------------------------

fn apps_tl() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

fn example_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(apps_tl())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("app.tl").is_file())
        .collect();
    dirs.sort();
    assert!(dirs.len() >= 6, "{dirs:?}");
    dirs
}

/// A scratch directory of package directories, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tilolang-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn package(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), text).unwrap();
        }
        dir
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn imports_are_found_among_sibling_packages_and_compiled_first() {
    let board = apps_tl().join("board");
    let order = dependency_order(&board, &[]).unwrap();
    let names: Vec<String> = order
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("board"));
    for dep in ["notes", "todo", "chat"] {
        assert!(names.contains(&dep.to_string()), "{names:?}");
    }
    assert_eq!(names.len(), 4, "only what board imports: {names:?}");
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(built.last().unwrap().compiled.meta.app, "so.tilo.board");
}

#[test]
fn every_example_package_builds_from_its_directory_with_its_committed_ids() {
    for dir in example_dirs() {
        let order = dependency_order(&dir, &[]).unwrap();
        let built = compile_dirs(&order, &[], &mut SeededEntropy(1))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        for b in &built {
            let committed = std::fs::read_to_string(b.dir.join("app.ids")).unwrap();
            assert_eq!(
                b.compiled.ids.to_text(),
                committed,
                "{}: app.ids is out of date",
                b.dir.display()
            );
        }
    }
}

#[test]
fn a_missing_import_names_the_app_and_where_it_looked() {
    let scratch = Scratch::new("missing");
    let dir = scratch.package("a", &[("app.tl", "app t.a \"A\"\n\nuse t.nowhere as n\n")]);
    let error = dependency_order(&dir, &[]).unwrap_err();
    assert!(
        error.contains("`use t.nowhere`") && error.contains("--path"),
        "{error}"
    );
}

#[test]
fn a_path_names_a_package_outside_the_siblings() {
    let scratch = Scratch::new("path");
    let lib = scratch.package(
        "elsewhere/lib",
        &[(
            "types.tl",
            "app t.lib \"Lib\"\n\ntype Item {\n    title: Text\n}\n",
        )],
    );
    let dir = scratch.package(
        "apps/main",
        &[
            ("main.tl", "app t.main \"Main\"\n\nuse t.lib as lib\n"),
            (
                "views.tl",
                "view show(i: lib.Item) {\n    text(i.title)\n}\n",
            ),
        ],
    );
    assert!(dependency_order(&dir, &[]).is_err(), "not a sibling");
    let order = dependency_order(&dir, std::slice::from_ref(&lib)).unwrap();
    assert_eq!(order, vec![lib, dir]);
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(built[1].compiled.meta.views[0].name, "show");
}

#[test]
fn import_cycles_are_refused() {
    let scratch = Scratch::new("cycle");
    let a = scratch.package("a", &[("app.tl", "app t.a \"A\"\n\nuse t.b as b\n")]);
    scratch.package("b", &[("app.tl", "app t.b \"B\"\n\nuse t.a as a\n")]);
    let error = dependency_order(&a, &[]).unwrap_err();
    assert!(error.contains("cycle"), "{error}");
}

#[test]
fn a_build_error_in_a_directory_names_the_file() {
    let scratch = Scratch::new("err");
    let dir = scratch.package(
        "p",
        &[
            ("a.tl", "app t.p \"P\"\n"),
            ("b.tl", "fn f() -> Int {\n    return nope\n}\n"),
        ],
    );
    let error = compile_dirs(std::slice::from_ref(&dir), &[], &mut SeededEntropy(1)).unwrap_err();
    let BuildError::Diagnostics { diags, .. } = &error else {
        panic!("{error}");
    };
    let expected = dir.join("b.tl").display().to_string();
    assert_eq!(diags[0].file.as_deref(), Some(expected.as_str()));
    assert!(
        error.to_string().starts_with(&format!("{expected}:2:")),
        "{error}"
    );
}

#[test]
fn the_stamp_changes_with_a_source_file_and_not_with_the_ids_lock() {
    let scratch = Scratch::new("stamp");
    let dir = scratch.package("p", &[("a.tl", "app t.p \"P\"\n")]);
    let dirs = vec![dir.clone()];
    let before = Stamp::of(&dirs);
    std::fs::write(dir.join("app.ids"), "x = abcdefgh\n").unwrap();
    assert_eq!(Stamp::of(&dirs), before);
    std::fs::write(dir.join("b.tl"), "fn f() -> Int {\n    return 1\n}\n").unwrap();
    assert_ne!(Stamp::of(&dirs), before);
}
