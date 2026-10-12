use super::*;

fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

fn fresh(language: Language, text: &[String]) -> Vec<Vec<Span>> {
    let mut state = highlighter(language).start();
    text.iter().map(|line| state.highlight_line(line)).collect()
}

fn check_sequence(sequence: &[(Language, &str)]) {
    let mut cache = IncrementalHighlights::default();
    let mut old = Vec::new();
    let mut spans = Vec::new();
    for (language, text) in sequence {
        let next = lines(text);
        cache.update(*language, &old, &next, &mut spans);
        assert_eq!(
            spans,
            fresh(*language, &next),
            "full-fresh oracle mismatch for {language:?}: {text:?}"
        );
        assert_eq!(cache.states.len(), next.len());
        old = next;
    }
}

#[test]
fn incremental_highlights_match_a_fresh_parse_across_edit_sequences() {
    // Each phase starts from an empty cache and checks every step against the full-fresh oracle.
    prefix_suffix_insert_delete_and_undo_style_reverts_match_fresh();
    comment_string_and_scope_changes_reparse_until_state_converges();
    unicode_newline_and_language_switches_match_fresh();
}

#[test]
fn hardware_comment_edits_and_dialect_switches_match_fresh_highlights() {
    check_sequence(&[
        (Language::Verilog, "module mac;\nassign out = 8;\nendmodule"),
        (
            Language::Verilog,
            "module mac;\n/*\nassign out = 8;\nendmodule",
        ),
        (
            Language::Verilog,
            "module mac;\n/*\nassign out = 8;\n*/\nendmodule",
        ),
        (Language::Verilog, "module mac;\nassign out = 8;\nendmodule"),
        (
            Language::SystemVerilog,
            "module mac;\nlogic valid;\nendmodule",
        ),
        (
            Language::SystemVerilog,
            "module mac;\n/* logic valid; */\nendmodule",
        ),
        (
            Language::PlainText,
            "module mac;\n/* logic valid; */\nendmodule",
        ),
    ]);
}

fn prefix_suffix_insert_delete_and_undo_style_reverts_match_fresh() {
    check_sequence(&[
        (Language::Rust, "fn main() {\n    let n = 1;\n}\n"),
        (Language::Rust, "fn main() {\n    let number = 1;\n}\n"),
        (
            Language::Rust,
            "fn main() {\n    let number = 1;\n    // inserted\n}\n",
        ),
        (Language::Rust, "fn main() {\n    // inserted\n}\n"),
        (Language::Rust, "fn main() {\n    let n = 1;\n}\n"),
        (Language::Rust, "// remote replacement\nfn other() {}\n"),
        (Language::Rust, ""),
        (Language::Rust, "\n\n"),
        (Language::Rust, "\n"),
    ]);
}

fn comment_string_and_scope_changes_reparse_until_state_converges() {
    check_sequence(&[
        (
            Language::Rust,
            "/* comment\ninside\n*/\nlet n = 1;\nlet x = 2;",
        ),
        (
            Language::Rust,
            "/* comment\ninside\n\nlet n = 1;\nlet x = 2;",
        ),
        (
            Language::Rust,
            "/* comment\ninside\n*/\nlet n = 1;\nlet x = 2;",
        ),
        (
            Language::Rust,
            "let text = r#\"multi\nstring\"#;\nlet x = 2;",
        ),
        (
            Language::Rust,
            "let text = r##\"multi\nstring\"#;\nlet x = 2;",
        ),
        (
            Language::Python,
            "text = '''multi\nstring\n'''\nprint(text)",
        ),
        (Language::Python, "text = 'multi\nstring\n'''\nprint(text)"),
        (
            Language::JavaScript,
            "let s = `hello\n${42}\nworld`;\nconsole.log(s);",
        ),
        (
            Language::JavaScript,
            "let s = `hello\n${\nworld`;\nconsole.log(s);",
        ),
        (
            Language::Html,
            "<script>\nlet s = 'text';\n</script>\n<p>text</p>",
        ),
        (
            Language::Html,
            "<style>\nlet s = 'text';\n</script>\n<p>text</p>",
        ),
    ]);
}

fn unicode_newline_and_language_switches_match_fresh() {
    let rust = "fn café() {\n    let s = \"日本語 🦀\";\n}\n";
    check_sequence(&[
        (Language::Rust, rust),
        (
            Language::Rust,
            "fn café() {\n    let s = \"日本語 🦀\";\n}\n//é",
        ),
        (
            Language::Rust,
            "fn café() {\n    let s = \"日本\n語 🦀\";\n}\n//é",
        ),
        (Language::PlainText, rust),
        (Language::Rust, rust),
        (Language::Python, rust),
    ]);
}

#[test]
fn incremental_highlights_reparse_only_until_grammar_state_converges() {
    // Each phase builds a fresh cache and bounds the number of tokenized lines.
    same_text_is_free_but_language_change_reparses_every_line();
    unchanged_suffix_text_is_not_sufficient_when_multiline_state_changes();
    representative_fixture_prepend_and_revert_reuse_the_unaffected_functions();
}

fn same_text_is_free_but_language_change_reparses_every_line() {
    let source = lines("fn main() {\nlet x = 1;\n}\n");
    let mut cache = IncrementalHighlights::default();
    let mut spans = Vec::new();
    assert_eq!(
        cache.update(Language::Rust, &[], &source, &mut spans),
        source.len()
    );
    assert_eq!(
        cache.update(Language::Rust, &source, &source, &mut spans),
        0
    );
    assert_eq!(
        cache.update(Language::PlainText, &source, &source, &mut spans),
        source.len()
    );
    assert_eq!(spans, fresh(Language::PlainText, &source));
}

fn unchanged_suffix_text_is_not_sufficient_when_multiline_state_changes() {
    let original = lines("/*\ninside\n*/\nlet x = 1;");
    let changed = lines("//\ninside\n*/\nlet x = 1;");
    let mut cache = IncrementalHighlights::default();
    let mut spans = Vec::new();
    cache.update(Language::Rust, &[], &original, &mut spans);
    let parsed = cache.update(Language::Rust, &original, &changed, &mut spans);
    assert!(
        parsed > 1,
        "different incoming grammar state forbids immediate suffix reuse"
    );
    assert_eq!(spans, fresh(Language::Rust, &changed));
}

fn representative_fixture_prepend_and_revert_reuse_the_unaffected_functions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.rs");
    crate::native_fixture::seed(&path).unwrap();
    let baseline = lines(&std::fs::read_to_string(path).unwrap());
    let changed = lines(&format!("x{}", baseline.join("\n")));
    let mut cache = IncrementalHighlights::default();
    let mut spans = Vec::new();
    cache.update(Language::Rust, &[], &baseline, &mut spans);
    let inserted = cache.update(Language::Rust, &baseline, &changed, &mut spans);
    assert_eq!(spans, fresh(Language::Rust, &changed));
    assert!(
        inserted <= 8,
        "prepend reparsed {inserted} lines instead of stopping after the changed function"
    );
    let restored = cache.update(Language::Rust, &changed, &baseline, &mut spans);
    assert_eq!(spans, fresh(Language::Rust, &baseline));
    assert!(
        restored <= 8,
        "revert reparsed {restored} lines instead of stopping after the changed function"
    );
}

#[test]
fn background_batches_match_continuous_grammars_and_cancel_replaced_work() {
    let pumped = ui::rt::background::Pumped::new();
    let background = pumped.background();
    let _scope = background.enter();
    for (language, opening, closing) in [
        (Language::Rust, "/*", "*/ let outside = 1;"),
        (Language::Python, "text = '''", "'''\nprint(text)"),
        (
            Language::JavaScript,
            "let text = `",
            "`;\nconsole.log(text);",
        ),
    ] {
        let text = std::iter::once(opening.to_owned())
            .chain((0..30).map(|line| format!("different source line {line}")))
            .chain([closing.to_owned(), String::new()])
            .collect::<Vec<_>>()
            .join("\n");
        let lines = std::sync::Arc::new(lines(&text));
        let mut job = BackgroundHighlights::start(
            language,
            std::sync::Arc::clone(&lines),
            ui::WakeHandle::new(),
        )
        .unwrap();
        let mut spans = Vec::new();
        assert_eq!(pumped.run_pending(), 1);
        assert!(job.collect(&mut spans));
        assert_eq!(spans.len(), 8);
        while {
            pumped.run_pending();
            job.collect(&mut spans)
        } {}
        assert_eq!(
            spans,
            fresh(language, &lines),
            "every row agrees with the real continuous grammar"
        );
        let abandoned =
            BackgroundHighlights::start(language, lines, ui::WakeHandle::new()).unwrap();
        drop(abandoned);
        assert_eq!(pumped.run_pending(), 1);
        assert!(
            pumped.is_idle(),
            "replacing a buffer schedules no stale continuation"
        );
    }
}
