//! The parsers on real output: cargo's human and JSON diagnostics as rustc
//! printed them, nextest and libtest results, git status.

use super::*;

const CARGO_ERROR: &str = include_str!("fixtures/cargo-error.txt");
const CARGO_WARNING: &str = include_str!("fixtures/cargo-warning.txt");
const CARGO_JSON: &str = include_str!("fixtures/cargo-json.txt");
const NEXTEST: &str = include_str!("fixtures/nextest.txt");
const GIT_STATUS: &str = include_str!("fixtures/git-status.txt");
const GIT_STATUS_SHORT: &str = include_str!("fixtures/git-status-short.txt");

fn kinds(items: &[RunItem], kind: ItemKind) -> Vec<&RunItem> {
    items.iter().filter(|item| item.kind == kind).collect()
}

#[test]
fn the_tool_follows_the_command() {
    assert_eq!(tool_of("cargo test -p sheets"), "cargo");
    assert_eq!(tool_of("RUST_LOG=debug cargo build"), "cargo");
    assert_eq!(tool_of("git status --short"), "git-status");
    assert_eq!(tool_of("git log"), "");
    assert_eq!(tool_of("ls -l"), "");
}

#[test]
fn rustc_diagnostics_in_the_human_format_carry_code_and_location() {
    let found = items("cargo check -p terminal", CARGO_ERROR.as_bytes());
    let errors = kinds(&found, ItemKind::Error);
    assert_eq!(errors.len(), 1, "{found:#?}");
    let error = errors[0];
    assert_eq!(error.text, "mismatched types");
    assert_eq!(error.code.as_deref(), Some("E0308"));
    assert_eq!(error.path.as_deref(), Some("apps/terminal/src/parse.rs"));
    assert_eq!((error.line, error.column), (Some(389), Some(5)));
    assert!(
        kinds(&found, ItemKind::Location).is_empty(),
        "the diagnostic's own location is not listed twice: {found:#?}"
    );

    let found = items("cargo check -p terminal", CARGO_WARNING.as_bytes());
    let warnings = kinds(&found, ItemKind::Warning);
    assert_eq!(warnings.len(), 1, "{found:#?}");
    assert_eq!(warnings[0].text, "unused variable: `unused`");
    assert_eq!(warnings[0].line, Some(388));
}

#[test]
fn rustc_diagnostics_in_json_carry_code_and_primary_span() {
    let found = items("cargo check --message-format=json", CARGO_JSON.as_bytes());
    let errors = kinds(&found, ItemKind::Error);
    let warnings = kinds(&found, ItemKind::Warning);
    assert_eq!((errors.len(), warnings.len()), (1, 1), "{found:#?}");
    assert_eq!(errors[0].code.as_deref(), Some("E0308"));
    assert_eq!(
        errors[0].line,
        Some(389),
        "the primary span, not the return type"
    );
    assert_eq!(warnings[0].text, "unused variable: `unused`");
}

#[test]
fn test_results_are_one_item_per_test() {
    let found = items("cargo test -p terminal --lib fixture_", NEXTEST.as_bytes());
    let failed = kinds(&found, ItemKind::TestFailed);
    assert_eq!(failed.len(), 1, "{found:#?}");
    assert_eq!(
        failed[0].code.as_deref(),
        Some("parse::fixture_tests::fixture_fails")
    );
    let plain = "running 3 tests\n\
                 test a::passes ... ok\n\
                 test a::fails ... FAILED\n\
                 test a::skipped ... ignored, fixture\n";
    let found = libtest(plain);
    let outcomes: Vec<(ItemKind, &str)> = found
        .iter()
        .map(|item| (item.kind, item.code.as_deref().unwrap_or_default()))
        .collect();
    assert_eq!(
        outcomes,
        [
            (ItemKind::TestPassed, "a::passes"),
            (ItemKind::TestFailed, "a::fails"),
            (ItemKind::TestIgnored, "a::skipped"),
        ]
    );
    assert_eq!(
        rerun_test(
            "cargo test -p terminal --lib fixture_",
            "parse::fixture_tests::fixture_fails"
        )
        .as_deref(),
        Some("cargo test -p terminal -- --exact parse::fixture_tests::fixture_fails")
    );
    assert_eq!(rerun_test("make test", "a::fails"), None);
    assert_eq!(rerun_test("cargo test", "a; rm -rf /"), None);
}

#[test]
fn git_status_is_one_item_per_changed_path() {
    for (fixture, codes) in [
        (
            GIT_STATUS,
            ["new file", "renamed", "deleted", "modified", "??"].as_slice(),
        ),
        (GIT_STATUS_SHORT, ["A", "D", "M", "R", "??"].as_slice()),
    ] {
        let found = items("git status", fixture.as_bytes());
        let changes = kinds(&found, ItemKind::Change);
        let paths: Vec<&str> = changes
            .iter()
            .filter_map(|item| item.path.as_deref())
            .collect();
        for path in [
            "fresh.txt",
            "new.txt",
            "gone.txt",
            "kept.txt",
            "untracked.txt",
        ] {
            assert!(paths.contains(&path), "{path} in {paths:?}");
        }
        let mut seen: Vec<&str> = changes
            .iter()
            .filter_map(|item| item.code.as_deref())
            .collect();
        seen.sort_unstable();
        let mut codes = codes.to_vec();
        codes.sort_unstable();
        assert_eq!(seen, codes);
    }
}

#[test]
fn every_run_gets_links_locations_and_errors_without_escapes() {
    let output = "\x1b[1;31merror\x1b[0m: build failed, see https://ci.example/run/7.\r\n\
                  at src/main.rs:12:4 and ./lib/x.py:3\n\
                  progress 10%\rprogress 100%\n";
    assert_eq!(
        strip_ansi(output),
        "error: build failed, see https://ci.example/run/7.\nat src/main.rs:12:4 and ./lib/x.py:3\nprogress 100%\n"
    );
    let found = items("make", output.as_bytes());
    let url = kinds(&found, ItemKind::Url);
    assert_eq!(url[0].path.as_deref(), Some("https://ci.example/run/7"));
    let locations: Vec<(Option<&str>, Option<u32>, Option<u32>)> =
        kinds(&found, ItemKind::Location)
            .iter()
            .map(|item| (item.path.as_deref(), item.line, item.column))
            .collect();
    assert_eq!(
        locations,
        [
            (Some("src/main.rs"), Some(12), Some(4)),
            (Some("./lib/x.py"), Some(3), None)
        ]
    );
    assert_eq!(kinds(&found, ItemKind::Error).len(), 1);
}

/// "Open the second diagnostic" finds it among a run's items and opens its
/// file at its line, relative to where the run ran; a link opens as a page.
#[test]
fn an_item_shows_its_file_at_its_line_or_its_page() {
    let output = format!(
        "{CARGO_WARNING}{CARGO_ERROR}see https://doc.rust-lang.org/error_codes/E0308.html\n"
    );
    let found = items("cargo check -p terminal", output.as_bytes());
    let diagnostics: Vec<usize> = found
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item.kind, ItemKind::Error | ItemKind::Warning))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(diagnostics.len(), 2, "{found:#?}");
    assert_eq!(
        shown("/src/tilo", &found, diagnostics[1]),
        Ok(Shown::File {
            path: "/src/tilo/apps/terminal/src/parse.rs".into(),
            line: Some(389),
            column: Some(5),
        })
    );
    let link = found
        .iter()
        .position(|item| item.kind == ItemKind::Url)
        .expect("the link is an item");
    assert_eq!(
        shown("/src/tilo", &found, link),
        Ok(Shown::Page(
            "https://doc.rust-lang.org/error_codes/E0308.html".into()
        ))
    );
    assert!(shown("/src/tilo", &found, found.len()).is_err());
}
