//! Packages of several files: one module, the `app` header in exactly one file, any file order,
//! diagnostics that name their file, and the directory tools (`fmt`, imports among siblings).

use std::path::{Path, PathBuf};

use tilolang::cli::format_package;
use tilolang::source::{
    BuildError, Stamp, catalogue_current, compile_dirs, compile_package, dependency_order,
    ids_current, search_of,
};
use tilolang::{Compiled, Diagnostic, IdsLock, PackageSource, SeededEntropy};

use crate::common::fixture;

const HEAD: &str = "app so.tilo.counter \"Counter\"\n\n/// A counter that several people can bump at once.\ntype Counter {\n    /// The current value.\n    value: Int merge counter\n}\n";
const ACTIONS: &str =
    "/// Add one to the counter.\naction increment(c: Counter) {\n    c.value += 1\n}\n";
const VIEWS: &str = "view counter(c: Counter) {\n    stack {\n        text(\"{c.value}\")\n        button(\"+1\", on: increment(c)) as plus\n    }\n}\n";

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
    // The code is the same module; the lines beside it name the files each declaration is
    // written in, which one written in a single file does not have.
    assert_eq!(
        tilolang::meta::without_section(&split.wasm, tilolang::meta::LINES_SECTION),
        tilolang::meta::without_section(&whole.wasm, tilolang::meta::LINES_SECTION)
    );
    let split_lines = tilolang::meta::lines_from_wasm(&split.wasm);
    assert!(
        split_lines.iter().any(|line| line.file == "b.tl"),
        "the action is placed in the file it is written in: {split_lines:?}"
    );
    assert!(
        tilolang::meta::lines_from_wasm(&whole.wasm)
            .iter()
            .all(|line| line.file == "/"),
        "one file's declarations are all placed in it"
    );
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
    let broken = "view counter(c: Counter) {\n    stack {\n        text(c.missing)\n    }\n}\n";
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
fn fmt_keeps_plain_comments_above_what_they_explain() {
    let out = format_package(&files(&[
        (
            "a.tl",
            "app so.tilo.counter \"Counter\"\n// the counter, above its declaration\ntype Counter {\n    /// How far it counts.\n    // merged by adding\n    value: Int merge counter\n}\n",
        ),
        (
            "b.tl",
            "action increment(c: Counter) {\n    // why: counters merge\n    c.value += 1\n    // and here the action ends\n}\nview show(c: Counter) {\n    let maybe = .some(c.value)\n    match maybe {\n        // one value, said\n        .some(x) => text(\"{x}\"),\n        .none => text(\"none\"),\n    }\n}\n",
        ),
    ]))
    .unwrap();
    let a = &out.files.iter().find(|f| f.0 == "a.tl").unwrap().2;
    assert_eq!(
        a,
        "app so.tilo.counter \"Counter\"\n\n// the counter, above its declaration\ntype Counter {\n    /// How far it counts.\n    // merged by adding\n    value: Int merge counter\n}\n"
    );
    let b = &out.files.iter().find(|f| f.0 == "b.tl").unwrap().2;
    assert_eq!(
        b,
        "action increment(c: Counter) {\n    // why: counters merge\n    c.value += 1\n    // and here the action ends\n}\n\nview show(c: Counter) {\n    let maybe = .some(c.value)\n    match maybe {\n        // one value, said\n        .some(x) => text(\"{x}\"),\n        .none => text(\"none\"),\n    }\n}\n"
    );
    // Canonical text formats to itself.
    let again = format_package(&files(&[("a.tl", a), ("b.tl", b)])).unwrap();
    for (_, old, new) in &again.files {
        assert_eq!(old, new);
    }
}

#[test]
fn fmt_keeps_a_comment_above_the_require_it_explains() {
    let out = format_package(&files(&[(
        "a.tl",
        "app so.tilo.counter \"Counter\"\naction increment(c: Counter) {\n    // only upward, so a counter never goes back\n    // and the reason says why\n    require c.value >= 0 \"The counter cannot go back\"\n    c.value += 1\n}\n",
    )]))
    .unwrap();
    let a = &out.files[0].2;
    assert_eq!(
        a,
        "app so.tilo.counter \"Counter\"\n\naction increment(c: Counter) {\n    // only upward, so a counter never goes back\n    // and the reason says why\n    require c.value >= 0 \"The counter cannot go back\"\n    c.value += 1\n}\n"
    );
    // Canonical text formats to itself.
    let again = format_package(&files(&[("a.tl", a)])).unwrap();
    assert_eq!(&again.files[0].2, a);
}

#[test]
fn fmt_keeps_a_paragraph_break_in_a_doc_comment() {
    let out = format_package(&files(&[(
        "a.tl",
        "app so.tilo.counter \"Counter\"\n/// The counter.\n///\n/// It counts up.\n/// A blank line\n\n/// between paragraphs is a break.\ntype Counter {\n    value: Int merge counter\n}\n",
    )]))
    .unwrap();
    let a = &out.files[0].2;
    assert_eq!(
        a,
        "app so.tilo.counter \"Counter\"\n\n/// The counter.\n///\n/// It counts up.\n/// A blank line\n///\n/// between paragraphs is a break.\ntype Counter {\n    value: Int merge counter\n}\n"
    );
    // Canonical text formats to itself.
    let again = format_package(&files(&[("a.tl", a)])).unwrap();
    assert_eq!(&again.files[0].2, a);
}

#[test]
fn fmt_places_comments_inside_expression_blocks_above_their_statement() {
    let out = format_package(&files(&[(
        "a.tl",
        "app so.tilo.counter \"Counter\"\nfn label(c: Counter) -> Text {\n    let shown = if c.value > 0 {\n        // it counts, so say how far\n        text(\"{c.value}\")\n        // and nothing else\n    } else {\n        text(\"none\")\n    }\n    // a short block stays on one line unless it holds a comment\n    let kept = { 1 }\n    return shown\n}\n",
    )]))
    .unwrap();
    let a = &out.files[0].2;
    assert_eq!(
        a,
        "app so.tilo.counter \"Counter\"\n\nfn label(c: Counter) -> Text {\n    let shown = if c.value > 0 {\n        // it counts, so say how far\n        text(\"{c.value}\")\n        // and nothing else\n    } else { text(\"none\") }\n    // a short block stays on one line unless it holds a comment\n    let kept = { 1 }\n    return shown\n}\n"
    );
    let again = format_package(&files(&[("a.tl", a)])).unwrap();
    assert_eq!(&again.files[0].2, a);
}

#[test]
fn fmt_places_comments_inside_a_plain_match_above_its_arm() {
    let out = format_package(&files(&[(
        "a.tl",
        "app so.tilo.counter \"Counter\"\nfn label(c: Counter) -> Text {\n    let maybe = .some(c.value)\n    return match maybe {\n        // one value, said\n        .some(x) => text(\"{x}\"),\n        // and none below\n        .none => {\n            // nothing to say\n            text(\"none\")\n        },\n        // after the last arm\n    }\n}\n",
    )]))
    .unwrap();
    let a = &out.files[0].2;
    assert_eq!(
        a,
        "app so.tilo.counter \"Counter\"\n\nfn label(c: Counter) -> Text {\n    let maybe = .some(c.value)\n    return match maybe {\n        // one value, said\n        .some(x) => text(\"{x}\"),\n        // and none below\n        .none => {\n            // nothing to say\n            text(\"none\")\n        }\n        // after the last arm\n    }\n}\n"
    );
    let again = format_package(&files(&[("a.tl", a)])).unwrap();
    assert_eq!(&again.files[0].2, a);
}

#[test]
fn the_std_package_is_in_canonical_form() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("std");
    let source = PackageSource::read_dir(&dir).unwrap();
    let out = format_package(&source).unwrap_or_else(|d| panic!("{}: {d:#?}", dir.display()));
    assert!(!out.files.is_empty());
    for (path, old, new) in &out.files {
        assert_eq!(
            old,
            new,
            "{path} is not in canonical form: run `tilolang fmt {}`",
            dir.display()
        );
    }
}

#[test]
fn the_example_packages_are_in_canonical_form() {
    for dir in example_dirs() {
        let source = PackageSource::read_dir(&dir).unwrap();
        let out = format_package(&source).unwrap_or_else(|d| panic!("{}: {d:#?}", dir.display()));
        for (path, old, new) in &out.files {
            assert_eq!(
                old,
                new,
                "{path} is not in canonical form: run `tilolang fmt {}`",
                dir.display()
            );
        }
    }
}

/// The packages written against a native app's published types (`use so.tilo.session as
/// session`): the compiler alone has none to compile against. `tilolang-cli`'s tests, which
/// have them, check that what these write is what is committed beside them.
const NEEDS_PUBLISHED_APPS: &[&str] = &["shell"];

/// The files a package's sources derive — its ids lock and its source catalogue — are what the
/// ones committed beside them say. A change to how they are derived (a description's first
/// sentence, a name in words, the words a view shows) changes every package that has them, so
/// it fails here, in the crate the derivation lives in, and not on main once it has landed.
#[test]
fn the_example_packages_hold_the_files_their_sources_write() {
    let dirs = example_dirs();
    let mut checked = Vec::new();
    for dir in &dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if NEEDS_PUBLISHED_APPS.contains(&name.as_str()) {
            continue;
        }
        let built = compile_package(dir, &search_of(dir, &[]), &[], &mut SeededEntropy(0))
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        assert_eq!(
            built.last().map(|b| &b.dir),
            Some(dir),
            "the root compiles last"
        );
        for package in &built {
            let package_dir = package.dir.display();
            assert!(
                ids_current(package),
                "{package_dir}/app.ids is not what its sources write; write it with \
                 `tilolang check --write-ids {package_dir}` and commit it with them"
            );
            assert!(
                catalogue_current(package),
                "{package_dir}/locales/en-US.ftl is not what its sources write; write it with \
                 `tilolang check --write-ids {package_dir}` and commit it with them"
            );
        }
        checked.push(name);
    }
    assert_eq!(
        checked.len() + NEEDS_PUBLISHED_APPS.len(),
        dirs.len(),
        "every package is checked or named above: {checked:?}"
    );
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
    let today = apps_tl().join("today");
    let order = dependency_order(&today, &[]).unwrap();
    let names: Vec<String> = order
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("today"));
    // `discussion` is not today's own import: tasks and notes use it, so it comes first.
    for dep in ["notes", "tasks", "chat", "discussion"] {
        assert!(names.contains(&dep.to_string()), "{names:?}");
    }
    assert_eq!(names.len(), 5, "only what today imports: {names:?}");
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        built.last().unwrap().compiled.meta.app,
        "so.tilo.lang.today"
    );
}

// Every example package building from its directory with the files its sources write is
// `tilolang-cli`'s `native_types::every_example_package_builds_from_its_directory_with_the_files_it_writes`:
// a package may `use` a native app (the shell calls the session's actions), and the native
// app's described package, which only the catalogue reads, is what compiling it needs.

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
fn the_agent_library_is_provided_like_the_host() {
    let scratch = Scratch::new("agent");
    let dir = scratch.package(
        "a",
        &[(
            "app.tl",
            "app t.a \"A\"\n\nuse tilo.agent as agent\n\ntype Tag {\n    title: Text\n}\n\naction who() -> Principal {\n    return agent.me\n}\n\naction what() -> Text {\n    return agent.describe(Tag)\n}\n",
        )],
    );
    let order = dependency_order(&dir, &[]).unwrap();
    assert_eq!(order, vec![dir]);
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(built[0].compiled.meta.uses, ["tilo.agent", "tilo.std"]);
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
                "view item(i: lib.Item) {\n    text(i.title)\n}\n",
            ),
        ],
    );
    assert!(dependency_order(&dir, &[]).is_err(), "not a sibling");
    let order = dependency_order(&dir, std::slice::from_ref(&lib)).unwrap();
    assert_eq!(order, vec![lib, dir]);
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(built[1].compiled.meta.views[0].name, "item");
}

#[test]
fn a_use_of_an_earlier_id_imports_the_renamed_package() {
    let scratch = Scratch::new("renamed");
    let tasks = scratch.package(
        "tasks",
        &[(
            "app.tl",
            "app t.tasks \"Tasks\" (previously: t.todo)\n\ntype Item {\n    title: Text\n}\n",
        )],
    );
    let source = "app t.main \"Main\"\n\nuse t.todo as todo\n\nview item(i: todo.Item) {\n    text(i.title)\n}\n";
    let dir = scratch.package("main", &[("app.tl", source)]);
    let order = dependency_order(&dir, &[]).unwrap();
    assert_eq!(order, vec![tasks, dir.clone()]);
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    let main = &built[1].compiled.meta;
    assert_eq!(
        main.uses,
        ["t.tasks", "tilo.std"],
        "imported under the id it has now, and the library every module compiles with"
    );
    assert_eq!(main.views[0].name, "item");
    // The importer's source stays as written.
    assert_eq!(std::fs::read_to_string(dir.join("app.tl")).unwrap(), source);
}

#[test]
fn an_id_a_package_still_has_is_its_own_not_a_renamed_ones() {
    let scratch = Scratch::new("still-there");
    scratch.package(
        "tasks",
        &[(
            "app.tl",
            "app t.tasks \"Tasks\" (previously: t.todo)\n\ntype Item {\n    title: Text\n}\n",
        )],
    );
    let todo = scratch.package(
        "todo",
        &[(
            "app.tl",
            "app t.todo \"Todo\"\n\ntype Item {\n    title: Text\n}\n",
        )],
    );
    let dir = scratch.package(
        "main",
        &[("app.tl", "app t.main \"Main\"\n\nuse t.todo as todo\n")],
    );
    let order = dependency_order(&dir, &[]).unwrap();
    assert_eq!(order, vec![todo, dir]);
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(built[1].compiled.meta.uses, ["t.todo", "tilo.std"]);
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

/// A library: a record type of its own, an enum, a function over its resource type, the action
/// that writes its records, and the view of its root.
const LIBRARY_PACKAGE: &str = r#"app test.marks "Marks"

/// How much a mark matters.
enum Importance {
    /// Low
    low
    /// High
    high
}

/// One mark on the board.
type Mark {
    title: Text
    importance: Importance
}

/// The board's marks.
type Marks {
    marks: Collection<Mark>
}

root marks: Marks

default view board for marks

/// How many marks the board holds.
fn count(m: Marks) -> Int {
    m.marks.len()
}

/// Adds a mark to the board.
action add(m: Marks, title: Text, importance: Importance) {
    m.marks.insert(Mark(title: title, importance: importance))
}

/// The board: its marks.
view board(m: Marks) {
    page {
        for mark in m.marks {
            text(mark.title)
        }
    }
}
"#;

/// The package that uses it: its own root and its own view, the library's action applied from a
/// action and its function called from the view.
const LIBRARY_USER: &str = r#"app test.notes "Notes"

use test.marks as marks

/// A note of the notebook.
type Note {
    title: Text
}

/// The notebook's notes.
type Notebook {
    notes: Collection<Note>
}

root notebook: Notebook

default view main for Notebook

/// Adds a high mark to the board through the library's action.
action add_high() {
    marks.add(marks.marks, "High", .high)
}

/// The notebook: its notes, and how many marks the board holds.
view main(n: Notebook) {
    page {
        for note in n.notes {
            text(note.title)
        }
        text("{marks.count(marks.marks)}")
        button("Add high", on: add_high())
    }
}
"#;

/// Names of the declarations of `meta`'s kind, in order.
fn names<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    items.collect()
}

/// A library package's declarations are as much a package's as any other's: a type, an enum, an
/// action, a function and a resource type, all of them under the library's app id, and a package
/// that uses it reaches them as `marks.*`.
#[test]
fn a_library_package_declares_types_fns_and_a_resource_of_its_own() {
    let scratch = Scratch::new("library");
    let library = scratch.package("marks", &[("app.tl", LIBRARY_PACKAGE)]);
    let notes = scratch.package("notes", &[("app.tl", LIBRARY_USER)]);
    let order = dependency_order(&notes, &[]).unwrap();
    assert_eq!(order, vec![library, notes]);
    let built = compile_dirs(&order, &[], &mut SeededEntropy(1)).unwrap_or_else(|e| panic!("{e}"));
    let (lib, user) = (&built[0].compiled, &built[1].compiled);

    // The library is a package: its declarations are its own, with ids locked in its `app.ids`.
    assert_eq!(lib.meta.app, "test.marks");
    assert_eq!(
        names(lib.meta.types.iter().map(|t| t.name.as_str())),
        ["Mark", "Marks"]
    );
    assert_eq!(
        names(lib.meta.enums.iter().map(|e| e.name.as_str())),
        ["Importance"]
    );
    assert_eq!(
        names(lib.meta.actions.iter().map(|a| a.name.as_str())),
        ["add"]
    );
    assert_eq!(
        names(lib.meta.fns.iter().map(|f| f.name.as_str())),
        ["count"]
    );
    assert_eq!(lib.meta.roots.len(), 1, "a resource of the library's own");
    for name in ["Mark", "Marks", "Importance", "add", "count"] {
        assert!(lib.ids.get(name).is_some(), "`{name}` is locked");
    }
    assert!(lib.ids.get("Mark.title").is_some());
    assert!(lib.ids.get("Importance.high").is_some());

    // The package that uses it reaches the same declarations, owned by the library.
    assert_eq!(user.meta.app, "test.notes");
    assert_eq!(user.meta.uses, ["test.marks", "tilo.std"]);
    assert_eq!(
        names(user.meta.actions.iter().map(|a| a.name.as_str())),
        ["add_high"],
        "the library's action is not copied"
    );
    assert!(
        user.meta.enums.is_empty(),
        "the library's enum is not copied"
    );
    for name in ["Mark", "Importance", "add", "count"] {
        assert!(user.ids.get(name).is_none(), "`{name}` stays the library's");
    }
    // Its own view and action compile against the library's names.
    assert_eq!(
        names(user.meta.offered_views().map(|v| v.name.as_str())),
        ["main"]
    );
}
