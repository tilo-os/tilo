//! A desk style is a view of the session's arrangement that any installed package declares
//! (`docs/designs/app-language/shell.md` §6). The shell's `desk_view` draws the person's chosen
//! style by `show(a, view: <style>)`, and the style is a view of a package other than the one
//! holding the arrangement type: `show` resolves it by the type its view takes, never by a list
//! in code. A style of the shell's own package is found by `views_of_type` as before; one of
//! another package is found here.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use tilolang_runtime::{AmStore, Meta, Runtime, SimpleAuthority, ViewInstance};

    /// The session's arrangement type, as the shell's package declares it: the origin is what
    /// makes a view of it a desk style.
    const SESSION: &str = r#"{
        "app": "so.tilo.session",
        "types": [
            {"id": "Arrangement", "name": "Arrangement", "origin": "session.arrangement", "fields": [
                {"id": "Arrangement.places", "name": "places", "type": "Collection<ArrangementPlace>"}
            ]},
            {"id": "ArrangementPlace", "name": "ArrangementPlace", "origin": "session.arrangement_place", "fields": [
                {"id": "ArrangementPlace.key", "name": "key", "type": "Text"},
                {"id": "ArrangementPlace.title", "name": "title", "type": "Text"}
            ]}
        ]
    }"#;

    /// The shell's package: it declares the arrangement type and one style of it of its own
    /// (`strips`), and the `desk_view` that draws the person's chosen style.
    const SHELL: &str = r#"app so.tilo.shell "Shell"

use so.tilo.session as session

type Arrangement (origin: session.arrangement) {
    places: Collection<ArrangementPlace>
}

type ArrangementPlace (origin: session.arrangement_place) {
    key: Text
    title: Text
}

root arrangement: Arrangement

default view desk_view for arrangement

/// The shipped strips style.
view strips(a: Arrangement) {
    stack {
        text("strips")
        for p in a.places {
            text(p.title)
        }
    }
}

/// What draws the desk: the style the person chose, by name. The `Viewing` record holds the
/// style as a package and a view (`me.styles:timeline`), and `show` finds it by that.
view desk_view(a: Arrangement) {
    show(a, view: "me.styles:timeline")
}

/// What a person's own style is drawn through, when its package is not loaded.
view unknown(a: Arrangement) {
    show(a, view: "me.styles:timeline")
}
"#;

    /// A person's own package, declaring a style of the shell's arrangement: a timeline of the
    /// places as cards. Its `ui.Card` never stands in the shell's own `strips`, so a tree with
    /// one proves the other package's view ran.
    const STYLE: &str = r#"app me.styles "My styles"

use so.tilo.shell as shell

/// My agents' work as a timeline.
view timeline(a: shell.Arrangement) {
    stack {
        text("timeline")
        for p in a.places {
            ui.Card(title: p.title) {
                text("a place")
            }
        }
    }
}
"#;

    fn session() -> Meta {
        meta(serde_json::from_str(SESSION).expect("the session's description"))
    }

    fn shell_app() -> App {
        App::compile(SHELL, &[session()])
    }

    fn style_app(shell: &App) -> App {
        App::compile(STYLE, &[session(), shell.meta().clone()])
    }

    /// The texts the tree draws, in order.
    fn drawn(view: &ViewInstance) -> Vec<String> {
        fn texts(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
            if node.kind == "ui.Text"
                && let Some(tilolang_runtime::Value::Text(text)) = node.props.get("value")
            {
                out.push(text.clone());
            }
            for child in &node.children {
                texts(child, out);
            }
        }
        let mut out = Vec::new();
        texts(view.tree(), &mut out);
        out
    }

    #[test]
    fn a_style_declared_by_another_package_draws_the_arrangement() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let shell = shell_app();
        shell.load(&rt);
        // The person's own package is installed too, so its `timeline` is declared here: the
        // `show` of `desk_view` finds it by the type its view takes.
        style_app(&shell).load(&rt);
        let root = arrangement_of(&rt, &shell);
        let view = rt
            .open_view(
                &me(),
                &shell.view("desk_view"),
                vec![Value::resource(&root)],
            )
            .unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            drawn(&view),
            ["timeline"],
            "the timeline's own view drew the arrangement, not the shell's strips: {}",
            dump(view.tree())
        );
    }

    #[test]
    fn a_style_whose_package_is_not_loaded_waits_for_it() {
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        let shell = shell_app();
        shell.load(&rt);
        // No `me.styles` here: `desk_view`'s `show` names a view of a package that is not loaded.
        // The call waits on it rather than faulting, and asks for that package.
        let root = arrangement_of(&rt, &shell);
        let view = rt
            .open_view(
                &me(),
                &shell.view("desk_view"),
                vec![Value::resource(&root)],
            )
            .unwrap_or_else(|why| panic!("{why}"));
        assert!(
            drawn(&view).is_empty(),
            "a style no package declares yet draws nothing: {}",
            dump(view.tree())
        );
        assert!(
            rt.wants_packages(),
            "the runtime asked for the package the style names"
        );
    }

    use tilolang_runtime::Value;

    /// The shell's `arrangement` root, made: what a desk style draws.
    fn arrangement_of(rt: &Runtime, shell: &App) -> tilolang_runtime::ResourceRef {
        let ty = shell
            .meta()
            .named_roots
            .iter()
            .find(|r| r.name == "arrangement")
            .map(|r| r.ty.clone())
            .expect("the shell declares its arrangement as a named root");
        rt.create_root(&ty).expect("the arrangement is made")
    }
}
