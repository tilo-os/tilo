//! layout.md: `space`, unit literals, layout primitives and sizing arguments.

use crate::common::{codes, compile_body, validate};

const LIBRARY: &str = "type Album {\n    title: Text\n    artist: Text\n}\ntype Library {\n    albums: Collection<Album>\n}\n\
view album_card(a: Album) {\n    layers {\n        image(a.title, aspect: 16/9)\n        text(a.artist, at: bottom_trailing)\n    }\n    text(a.title, lines: 1, truncate)\n    if not space.compact {\n        text(a.artist)\n    }\n}\n\
view library(l: Library) {\n    page {\n        heading(\"Albums\")\n        grid(min: 11rem) {\n            for a in l.albums {\n                album_card(a, fill(2))\n            }\n        }\n    }\n}\n\
view detail(a: Album) {\n    page(full) {\n        split(side: leading, collapse: drawer) {\n            primary(min: 18rem) {\n                text(a.title)\n            }\n            secondary(min: 22rem) {\n                scroll {\n                    text(a.artist, min: 4dp, max: 1.5rem)\n                }\n            }\n        }\n    }\n}\n\
view bar(a: Album) {\n    if space.compact {\n        row(distribute: between) { text(a.title); spacer }\n    } else {\n        row(gap: section) {\n            text(a.title)\n            spacer\n            text(a.artist, fill)\n        }\n    }\n    if space.width_class == wide and space.orientation != portrait {\n        text(\"{space.width}\")\n    }\n}";

fn contains(wasm: &[u8], needle: &str) -> bool {
    wasm.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn the_layout_examples_compile_and_validate() {
    let c = compile_body(LIBRARY);
    validate(&c.wasm);
}

#[test]
fn units_become_unit_tagged_float_props() {
    let c = compile_body(LIBRARY);
    for want in [
        "min.rem",
        "min.dp",
        "max.rem",
        "aspect",
        "fill",
        "lines",
        "truncate",
        "at",
        "bottom_trailing",
        "distribute",
        "between",
        "section",
    ] {
        assert!(
            contains(&c.wasm, want),
            "{want} is not in the module's strings"
        );
    }
}

#[test]
fn space_reads_use_the_space_imports() {
    let c = compile_body(LIBRARY);
    let mut names = Vec::new();
    for p in wasmparser::Parser::new(0).parse_all(&c.wasm) {
        if let wasmparser::Payload::ImportSection(r) = p.expect("parses") {
            names.extend(
                r.into_imports()
                    .map(|i| i.expect("import").name.to_string()),
            );
        }
    }
    assert!(
        names.contains(&"space_i".to_string()) && names.contains(&"space_f".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"ui_prop_f".to_string()));
}

#[test]
fn space_is_only_for_views() {
    let got = codes("fn f() -> Bool {\n    return space.compact\n}");
    assert_eq!(got, vec!["E064"]);
    assert_eq!(
        codes("view v() {\n    if space.nothing {\n        spacer\n    }\n}"),
        vec!["E021"]
    );
}

#[test]
fn tokens_are_values_only_as_named_ui_arguments() {
    assert_eq!(
        codes("fn f() -> Int {\n    let x = tight\n    return 1\n}"),
        vec!["E014"]
    );
    assert!(
        codes("view v() {\n    row(gap: tight, align: center) {\n        spacer\n    }\n}")
            .is_empty()
    );
}

#[test]
fn unknown_layout_arguments_list_the_valid_ones() {
    let err = tilolang::compile_package(
        "app t.x \"X\"\nview v() {\n    grid(nope: 1)\n}\n",
        &tilolang::IdsLock::new(),
        &[],
    )
    .expect_err("fails");
    assert_eq!(err[0].code, "E063");
    assert!(
        err[0].message.contains("min") && err[0].message.contains("columns"),
        "{}",
        err[0].message
    );
}

#[test]
fn min_and_max_need_a_unit() {
    assert_eq!(codes("view v() {\n    grid(min: 11)\n}"), vec!["E063"]);
}

#[test]
fn embed_takes_text_source_and_author_and_emits_one_node() {
    let c = compile_body(
        "type M {\n    who: Text\n    src: Text\n}\nview v(m: M) {\n    embed(m.src, author: m.who)\n}",
    );
    validate(&c.wasm);
    for want in ["embed", "source", "author"] {
        assert!(contains(&c.wasm, want), "{want} is not in the module");
    }
    assert!(
        codes("type M {\n    src: Text\n}\nview v(m: M) {\n    embed(m.src)\n}")
            .contains(&"E022".to_string()),
        "an embed without an author is refused"
    );
    assert_eq!(
        codes("type M {\n    n: Int\n}\nview v(m: M) {\n    embed(m.n, author: \"a\")\n}"),
        vec!["E020"]
    );
}
