//! layout.md: `space`, unit literals, layout primitives and sizing arguments.

use crate::common::{codes, compile_body, validate};

const LIBRARY: &str = "type Album {\n    title: Text\n    artist: Text\n}\ntype Library {\n    albums: Collection<Album>\n}\n\
view album_card(a: Album) {\n    layers {\n        image(a.title, aspect: 16/9)\n        text(a.artist, at: .bottom_trailing)\n    }\n    text(a.title, lines: 1, truncate)\n    if not space.compact {\n        text(a.artist)\n    }\n}\n\
view library(l: Library) {\n    page {\n        text(\"Albums\", role: .title)\n        grid(min: 11rem) {\n            for a in l.albums {\n                album_card(a, fill(2))\n            }\n        }\n    }\n}\n\
view detail(a: Album) {\n    canvas {\n        split(side: .leading, collapse: .drawer) {\n            primary(min: 18rem) {\n                text(a.title)\n            }\n            secondary(min: 22rem) {\n                scroll {\n                    text(a.artist, min: 4dp, max: 1.5rem)\n                }\n            }\n        }\n    }\n}\n\
view bar(a: Album) {\n    if space.compact {\n        row(distribute: .between) { text(a.title); spacer }\n    } else {\n        row(gap: .section) {\n            text(a.title)\n            spacer\n            text(a.artist, fill)\n        }\n    }\n    if space.width_class == .wide and space.orientation != .portrait {\n        text(\"{space.width}\")\n    }\n}";

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

// USER REQUIREMENT (2026-10-20): "layout/UI arguments become typed enums declared by the
// primitives (`gap: .section`, `align: .center`, ...); bare words are no longer accepted there
// (error suggesting the dot form). Unit literals (11rem) and flags stay."
#[test]
fn options_are_dot_literals_of_the_primitives_enums() {
    assert!(
        codes("view v() {\n    row(gap: .tight, align: .center, distribute: .between) {\n        spacer\n    }\n}")
            .is_empty()
    );
    assert!(
        codes("view v(b: Bool) {\n    stack(gap: if b { .tight } else { .group }) {\n        text(\"x\", role: .title)\n        text(\"y\", role: .caption, truncate, min: 11rem)\n    }\n}")
            .is_empty(),
        "an option can be chosen at run time; flags and units stay"
    );
    assert_eq!(
        codes("view v() {\n    row(gap: tight) {\n        spacer\n    }\n}"),
        vec!["E065"]
    );
    assert_eq!(
        codes("view v() {\n    row(align: .middle) {\n        spacer\n    }\n}"),
        vec!["E036"],
        "an option outside the primitive's enum is refused"
    );
    assert_eq!(
        codes("fn f() -> Int {\n    let x = tight\n    return 1\n}"),
        vec!["E014"]
    );
}

#[test]
fn a_chosen_option_reaches_the_host_as_its_name() {
    let c = compile_body(
        "view v(b: Bool) {\n    text(\"x\", role: if b { .numeric } else { .caption })\n}",
    );
    validate(&c.wasm);
    for want in ["numeric", "caption", "title", "body"] {
        assert!(
            contains(&c.wasm, want),
            "{want} is not in the module's strings"
        );
    }
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
fn embed_takes_text_source_a_principal_author_and_emits_one_node() {
    let c = compile_body(
        "type M {\n    who: Principal\n    src: Text\n    n: Int\n    run: Option<host.Invocation>\n}\nview v(m: M) {\n    embed(m.src, author: m.who, invocation: m.run)\n}",
    );
    validate(&c.wasm);
    for want in ["embed", "source", "author", "invocation"] {
        assert!(contains(&c.wasm, want), "{want} is not in the module");
    }
    let head = "type M {\n    who: Principal\n    src: Text\n    n: Int\n}\n";
    assert!(
        codes(&format!("{head}view v(m: M) {{\n    embed(m.src)\n}}"))
            .contains(&"E022".to_string()),
        "an embed without an author is refused"
    );
    assert_eq!(
        codes(&format!(
            "{head}view v(m: M) {{\n    embed(m.n, author: m.who)\n}}"
        )),
        vec!["E020"]
    );
    assert_eq!(
        codes(&format!(
            "{head}view v(m: M) {{\n    embed(m.src, author: m.src)\n}}"
        )),
        vec!["E020"],
        "text is not a principal"
    );
}
