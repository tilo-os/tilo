//! A style is a view of the session's layout that any installed package declares
//! (`docs/designs/app-language/shell.md` §6). The shell's `style_shell` draws the person's chosen
//! style by `show(a, view: <style>)`, and the style is a view of a package other than the one
//! holding the layout type: `show` resolves it by the type its view takes, never by a list
//! in code. A style of the shell's own package is found by `views_of_type` as before; one of
//! another package is found here.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Meta, Runtime, ViewInstance};

    /// The session's layout type, as the shell's package declares it: the origin is what
    /// makes a view of it a style.
    const SESSION: &str = r#"{
        "app": "so.tilo.session",
        "types": [
            {"id": "Layout", "name": "Layout", "origin": "session.layout", "fields": [
                {"id": "Layout.places", "name": "places", "type": "Collection<LayoutPlace>"}
            ]},
            {"id": "LayoutPlace", "name": "LayoutPlace", "origin": "session.layout_place", "fields": [
                {"id": "LayoutPlace.key", "name": "key", "type": "Text"},
                {"id": "LayoutPlace.title", "name": "title", "type": "Text"}
            ]}
        ]
    }"#;

    /// The shell's package: it declares the layout type and one style of it of its own
    /// (`strips`), and the `style_shell` that draws the person's chosen style.
    const SHELL: &str = r#"app so.tilo.shell "Shell"

use so.tilo.session as session

type Layout (origin: session.layout) {
    places: Collection<LayoutPlace>
}

type LayoutPlace (origin: session.layout_place) {
    key: Text
    title: Text
}

root layout: Layout

default view style_shell for layout

/// The shipped strips style.
view strips(a: Layout) {
    stack {
        text("strips")
        for p in a.places {
            text(p.title)
        }
    }
}

/// What draws the shell: the style the person chose, by name. The `Viewing` record holds the
/// style as a package and a view (`me.styles:timeline`), and `show` finds it by that.
view style_shell(a: Layout) {
    show(a, view: "me.styles:timeline")
}

/// What a person's own style is drawn through, when its package is not loaded.
view unknown(a: Layout) {
    show(a, view: "me.styles:timeline")
}
"#;

    /// A person's own package, declaring a style of the shell's layout: a timeline of the
    /// places as cards. Its `ui.Card` never stands in the shell's own `strips`, so a tree with
    /// one proves the other package's view ran.
    const STYLE: &str = r#"app me.styles "My styles"

use so.tilo.shell as shell

/// My agents' work as a timeline.
view timeline(a: shell.Layout) {
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

    tilo_test_env::scenario! {
        fn a_shell_style_is_drawn_by_the_package_that_declares_it;

        phase a_style_declared_by_another_package_draws_the_layout;
        {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let shell = shell_app();
            shell.load(&rt);
            // The person's own package is installed too, so its `timeline` is declared here: the
            // `show` of `style_shell` finds it by the type its view takes.
            style_app(&shell).load(&rt);
            let root = layout_of(&rt, &shell);
            let view = rt
                .open_view(
                    &me(),
                    &shell.view("style_shell"),
                    vec![Value::resource(&root)],
                )
                .unwrap_or_else(|why| panic!("{why}"));
            assert_eq!(
                drawn(&view),
                ["timeline"],
                "the timeline's own view drew the layout, not the shell's strips: {}",
                dump(view.tree())
            );
        }

        phase a_style_whose_package_is_not_loaded_waits_for_it;
        {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let shell = shell_app();
            shell.load(&rt);
            // No `me.styles` here: `style_shell`'s `show` names a view of a package that is not loaded.
            // The call waits on it rather than faulting, and asks for that package.
            let root = layout_of(&rt, &shell);
            let view = rt
                .open_view(
                    &me(),
                    &shell.view("style_shell"),
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

        phase a_style_whose_view_traps_draws_a_failed_state_and_is_reported;
        {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let shell = shell_naming("me.broken", "broken");
            shell.load(&rt);
            App::compile(TRAPPING, &[session(), shell.meta().clone()]).load(&rt);
            let root = layout_of(&rt, &shell);
            let view = rt
                .open_view(
                    &me(),
                    &shell.view("style_shell"),
                    vec![Value::resource(&root)],
                )
                .unwrap_or_else(|why| panic!("{why}"));
            assert_eq!(
                failed_state(&view).as_deref(),
                Some("This couldn't be shown"),
                "a trapping style draws a failed state where it stands: {}",
                dump(view.tree())
            );
            let reported = rt.take_view_failures();
            let said = reported
                .iter()
                .find(|failure| failure.app == "me.broken")
                .unwrap_or_else(|| panic!("the failure is reported: {reported:?}"));
            assert_eq!(said.view, "broken", "the view own name: {said:?}");
            assert!(!said.reason.is_empty(), "with why: {said:?}");
        }

        phase a_style_whose_package_does_not_load_draws_a_failed_state_and_is_reported;
        {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let shell = shell_naming("me.miscompiled", "miscompiled");
            shell.load(&rt);
            // The host tried the package and could not load it: nothing more is coming, so the
            // call draws a failed state rather than a skeleton that never fills.
            rt.app_failed("me.miscompiled", "the module does not load".to_owned());
            let root = layout_of(&rt, &shell);
            let view = rt
                .open_view(
                    &me(),
                    &shell.view("style_shell"),
                    vec![Value::resource(&root)],
                )
                .unwrap_or_else(|why| panic!("{why}"));
            assert_eq!(
                failed_state(&view).as_deref(),
                Some("This couldn't be shown"),
                "a style whose package did not load draws a failed state: {}",
                dump(view.tree())
            );
            let reported = rt.take_view_failures();
            let said = reported
                .iter()
                .find(|failure| failure.reason == "the module does not load")
                .unwrap_or_else(|| panic!("the failure is reported: {reported:?}"));
            assert_eq!(said.app, "me.miscompiled", "{said:?}");
            assert_eq!(said.view, "me.miscompiled:miscompiled", "{said:?}");
        }

        phase a_fixed_style_package_brings_the_style_back;
        {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let shell = shell_naming("me.broken", "broken");
            shell.load(&rt);
            // The package was tried and did not load: the shell shows its failed state.
            rt.app_failed("me.broken", "the module does not load".to_owned());
            let root = layout_of(&rt, &shell);
            let failed = rt
                .open_view(
                    &me(),
                    &shell.view("style_shell"),
                    vec![Value::resource(&root)],
                )
                .unwrap_or_else(|why| panic!("{why}"));
            assert!(failed_state(&failed).is_some(), "{}", dump(failed.tree()));
            rt.take_view_failures();
            // The package is fixed and installed again, and loads: its style draws again, live,
            // with no restart.
            App::compile(WORKING, &[session(), shell.meta().clone()]).load(&rt);
            let back = rt
                .open_view(
                    &me(),
                    &shell.view("style_shell"),
                    vec![Value::resource(&root)],
                )
                .unwrap_or_else(|why| panic!("{why}"));
            assert!(
                failed_state(&back).is_none(),
                "the fixed package's style draws again, no restart: {}",
                dump(back.tree())
            );
            assert_eq!(
                drawn(&back),
                ["a working style"],
                "the style's own words stand on the shell: {}",
                dump(back.tree())
            );
        }
    }

    use tilolang_runtime::Value;

    /// A style whose view traps when it runs: a division that is not a number, which the
    /// runtime refuses in the middle of the call (`shell.md` §6, "Nothing can lock a person
    /// out").
    const TRAPPING: &str = r#"app me.broken "A broken style"

use so.tilo.shell as shell

/// A style that fails.
view broken(a: shell.Layout) {
    let places = a.places.count()
    ui.Text("at {places / 0}")
}
"#;

    /// The same style after its package was fixed: a plain sentence, no trap.
    const WORKING: &str = r#"app me.broken "A working style"

use so.tilo.shell as shell

/// A style that works.
view broken(a: shell.Layout) {
    ui.Text("a working style")
}
"#;

    /// A shell whose `style_shell` names the style `view` of the package `app`.
    fn shell_naming(app: &str, view: &str) -> App {
        let source = format!(
            r#"app so.tilo.shell "Shell"

use so.tilo.session as session

type Layout (origin: session.layout) {{
    places: Collection<LayoutPlace>
}}

type LayoutPlace (origin: session.layout_place) {{
    key: Text
    title: Text
}}

root layout: Layout

default view style_shell for layout

/// The shipped strips style.
view strips(a: Layout) {{
    stack {{
        text("strips")
    }}
}}

/// What draws the shell: the style the person chose, by name.
view style_shell(a: Layout) {{
    show(a, view: "{app}:{view}")
}}
"#
        );
        App::compile(&source, &[session()])
    }

    /// The shell's `layout` root, made: what a style draws.
    fn layout_of(rt: &Runtime, shell: &App) -> tilolang_runtime::ResourceRef {
        let ty = shell
            .meta()
            .named_roots
            .iter()
            .find(|r| r.name == "layout")
            .map(|r| r.ty.clone())
            .expect("the shell declares its layout as a named root");
        rt.create_root(&ty).expect("the layout is made")
    }

    /// The shell a failed style shows: a failed state naming the view, drawn where the call
    /// stands, in place of a blank frame or an endless skeleton.
    fn failed_state(view: &ViewInstance) -> Option<String> {
        fn find(node: &tilolang_runtime::UiNode, out: &mut Option<String>) {
            if node.kind == "ui.EmptyState" {
                *out = Some(
                    node.props
                        .get("title")
                        .and_then(|t| match t {
                            tilolang_runtime::Value::Text(text) => Some(text.clone()),
                            _ => None,
                        })
                        .unwrap_or_default(),
                );
            }
            for child in &node.children {
                find(child, out);
            }
        }
        let mut out = None;
        find(view.tree(), &mut out);
        out
    }
}
