//! `protect capture`: the view-level declaration a capture or a stream leaves the view out of.

use crate::common::{codes, compile_body as compile, compile_with};
use tilolang::{parse, print_module};

fn print_of(src: &str) -> String {
    print_module(&parse(src).unwrap_or_else(|d| panic!("{d:#?}")))
}

fn protected_views(src: &str) -> Vec<String> {
    let compiled = compile(src);
    compiled
        .meta
        .views
        .iter()
        .filter(|view| view.capture_protected)
        .map(|view| view.id.clone())
        .collect()
}

tilo_test_env::scenario! {
    /// `protect capture` parses, prints back and reaches the view's metadata.
    fn a_view_protect_capture_is_declared_and_published;

    phase a_protected_view_is_printable_canonically;
    {
        let src = "app t.x \"X\"\n\ntype T {\n    n: Int\n}\n\nview totp(t: T) {\n    protect capture\n    text(\"{t.n}\")\n}\n";
        assert_eq!(print_of(src), src, "the declaration prints where it is written");
        assert_eq!(print_of(&print_of(src)), src);
    }

    phase the_views_metadata_carries_the_protection;
    {
        let ids = protected_views(
            "type T {\n    n: Int\n}\n\nview totp(t: T) {\n    protect capture\n    text(\"{t.n}\")\n}\n\nview counter(t: T) {\n    text(\"{t.n}\")\n}\n",
        );
        assert_eq!(ids.len(), 1, "one view declared it: {ids:?}");
        let compiled = compile("type T {\n    n: Int\n}\n\nview totp(t: T) {\n    protect capture\n    text(\"{t.n}\")\n}\n");
        let view = compiled
            .meta
            .views
            .iter()
            .find(|view| view.name == "totp")
            .expect("the view is published");
        assert!(view.capture_protected);
        assert!(compiled.meta.view_capture_protected(&view.id));
    }

    phase a_view_showing_a_protected_view_is_protected_too;
    {
        let ids = protected_views(
            "type T {\n    n: Int\n}\n\nview totp(t: T) {\n    protect capture\n    text(\"{t.n}\")\n}\n\nview board(t: T) {\n    stack {\n        totp(t)\n    }\n}\n",
        );
        assert_eq!(ids.len(), 2, "the view showing it too: {ids:?}");
    }

    phase a_protected_template_protects_the_view_it_expands_in;
    {
        let ids = protected_views(
            "type T {\n    n: Int\n}\n\ntemplate view totp(t: T) {\n    protect capture\n    text(\"{t.n}\")\n}\n\nview board(t: T) {\n    stack {\n        totp(t)\n    }\n}\n",
        );
        assert_eq!(ids.len(), 1, "the view it expands in: {ids:?}");
        let compiled = compile("type T {\n    n: Int\n}\n\ntemplate view totp(t: T) {\n    protect capture\n    text(\"{t.n}\")\n}\n\nview board(t: T) {\n    stack {\n        totp(t)\n    }\n}\n");
        let board = compiled
            .meta
            .views
            .iter()
            .find(|view| view.name == "board")
            .expect("the view is published");
        assert!(board.capture_protected);
    }

    phase only_capture_may_follow_protect;
    {
        assert_eq!(
            codes("type T {\n    n: Int\n}\nview v(t: T) {\n    protect streams\n    text(\"{t.n}\")\n}\n"),
            vec!["E146"]
        );
    }

    phase a_view_declares_protect_capture_once;
    {
        assert_eq!(
            codes("type T {\n    n: Int\n}\nview v(t: T) {\n    protect capture\n    protect capture\n    text(\"{t.n}\")\n}\n"),
            vec!["E147"]
        );
    }

    phase protect_capture_heads_the_view_and_is_no_statement_among_what_it_shows;
    {
        let src = "app t.x \"X\"\ntype T {\n    n: Int\n}\nview v(t: T) {\n    text(\"{t.n}\")\n    protect capture\n}\n";
        let diags = compile_with(src, &[]).expect_err("a statement it is not");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("protect capture")),
            "{diags:#?}"
        );
    }
}
