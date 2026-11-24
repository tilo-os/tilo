//! The snippets app (`apps/tl/snippets/app.tl`) end to end: making, starring, filing and tagging
//! snippets, copying one, filling in its variables before copying, renaming folders and tags, and
//! the window's places, rows and page.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::clipboard::ClipboardHost;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/snippets/app.tl");

    /// What the device's clipboard holds.
    #[derive(Clone, Default)]
    struct Selection {
        copied: Rc<RefCell<Option<String>>>,
    }

    impl ClipboardHost for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            if mime != "text/plain" {
                return false;
            }
            *self.copied.borrow_mut() = Some(String::from_utf8_lossy(bytes).into_owned());
            true
        }

        fn paste(&self, _types: &[&str]) -> Option<(String, Vec<u8>)> {
            None
        }
    }

    struct Snippets {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        selection: Selection,
    }

    impl Snippets {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let selection = Selection::default();
            rt.set_clipboard(selection.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            // Copying reaches the person's clipboard, outside Tilo, which the policy grants the
            // app it is written in.
            for action in ["copy", "copy_filled"] {
                authority.grant_outside(&app.action(action));
            }
            let lib = rt.create_root("Library").unwrap();
            Self {
                rt,
                app,
                lib,
                selection,
            }
        }

        fn call(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let id = self.app.action(action);
            let args = self.app.args(&self.rt, action, given);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn add(&self, title: &str, folder: &str) -> ResourceRef {
            returned_ref(self.call(
                "new_snippet",
                &[
                    ("lib", Value::resource(&self.lib)),
                    ("title", Value::text(title)),
                    ("folder", Value::text(folder)),
                ],
            ))
        }

        fn field(&self, at: &ResourceRef, field: &str) -> Json {
            self.rt.project(&Value::resource(at))["fields"][field].clone()
        }

        fn window(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap();
            window.set_lazy_rows(false).unwrap();
            window
        }

        fn snippets_window(&self) -> ViewInstance {
            self.window("snippets_window", vec![Value::resource(&self.lib)])
        }

        /// Types `text` into the snippet's body, as the page's editor does.
        fn write_body(&self, snippet: &ResourceRef, text: &str) {
            let mut page = self.window("snippet_page", vec![Value::resource(snippet)]);
            let editor = of_kind(page.tree(), "ui.CodeEditor")
                .into_iter()
                .next()
                .map(|editor| editor.key.clone())
                .unwrap_or_else(|| panic!("no body editor in {}", dump(page.tree())));
            page.edit(&editor, &Value::text(text)).unwrap();
        }

        fn copied(&self) -> String {
            self.selection
                .copied
                .borrow()
                .clone()
                .unwrap_or_else(|| panic!("nothing was copied"))
        }

        /// The snippet the list shows as `title`.
        fn listed(&self, title: &str) -> ResourceRef {
            let window = self.snippets_window();
            of_kind(window.tree(), "ui.ListItem")
                .into_iter()
                .filter_map(|row| match row.props.get("item") {
                    Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .find(|snippet| self.field(snippet, "title") == json!(title))
                .unwrap_or_else(|| panic!("no snippet called {title} in the list"))
        }
    }

    /// Every text the tree shows, in document order.
    fn drawn(view: &ViewInstance) -> Vec<String> {
        of_kind(view.tree(), "ui.Text")
            .into_iter()
            .filter_map(|node| match node.props.get("value") {
                Some(Value::Text(text)) => Some(text.clone()),
                Some(Value::Opt(Some(held))) => match &**held {
                    Value::Text(text) => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// The text of a node's property, whatever option carries it.
    fn text(node: &tilolang_runtime::UiNode, name: &str) -> String {
        match node.props.get(name) {
            Some(Value::Text(text)) => text.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(text) => text.clone(),
                other => panic!("{name} of {} is {other:?}", node.kind),
            },
            other => panic!("{name} of {} is {other:?}", node.kind),
        }
    }

    #[test]
    fn a_snippet_is_made_starred_filed_and_tagged() {
        let snippets = Snippets::new();
        let restart = snippets.add("Restart deployment", "Ops");
        snippets.call(
            "set_tags",
            &[
                ("snippet", Value::resource(&restart)),
                ("tags", Value::text("k8s, deploy")),
            ],
        );
        snippets.call("star", &[("snippet", Value::resource(&restart))]);
        let plain = snippets.add("Meeting note", "Writing");
        assert_eq!(
            snippets.field(&restart, "title"),
            json!("Restart deployment")
        );
        assert_eq!(snippets.field(&restart, "favorite"), json!(true));
        assert_eq!(snippets.field(&restart, "tags"), json!({"count": 2}));

        // The sidebar lists every place with what it holds, and the starred snippet stands
        // before the one it would otherwise follow.
        let window = snippets.snippets_window();
        let places: Vec<(String, f64)> = of_kind(window.tree(), "ui.NavItem")
            .into_iter()
            .map(|item| {
                let count = match item.props.get("count") {
                    Some(Value::Int(n)) => *n as f64,
                    Some(Value::Float(n)) => *n,
                    Some(Value::Opt(Some(held))) => match &**held {
                        Value::Int(n) => *n as f64,
                        Value::Float(n) => *n,
                        other => panic!("a count is {other:?}"),
                    },
                    other => panic!("a count is {other:?}"),
                };
                (text(item, "label"), count)
            })
            .collect();
        assert!(
            places.contains(&("Favorites".to_owned(), 1.0)),
            "{places:?}"
        );
        assert!(places.contains(&("Ops".to_owned(), 1.0)), "{places:?}");
        assert!(places.contains(&("Writing".to_owned(), 1.0)), "{places:?}");
        assert!(places.contains(&("k8s".to_owned(), 1.0)), "{places:?}");
        let rows: Vec<String> = of_kind(window.tree(), "ui.ListRow")
            .into_iter()
            .map(|row| text(row, "title"))
            .collect();
        assert_eq!(
            rows,
            ["Restart deployment", "Meeting note"],
            "{:?}",
            dump(window.tree())
        );
        assert_eq!(snippets.field(&plain, "favorite"), json!(false));
    }

    #[test]
    fn copying_a_snippet_puts_it_on_the_clipboard_and_counts_the_use() {
        let snippets = Snippets::new();
        let restart = snippets.add("Restart deployment", "Ops");
        snippets.write_body(
            &restart,
            "kubectl -n ${namespace} rollout restart deployment/${deployment}",
        );
        snippets
            .rt
            .invoke_action(
                &me(),
                (
                    snippets.app.action("copy").as_str(),
                    vec![Value::resource(&restart)],
                ),
            )
            .unwrap_or_else(|error| panic!("copy: {error:?}"));
        assert_eq!(
            snippets.copied(),
            "kubectl -n ${namespace} rollout restart deployment/${deployment}"
        );
        assert_eq!(snippets.field(&restart, "uses"), json!(1));
        let page = snippets.window("snippet_page", vec![Value::resource(&restart)]);
        assert!(
            drawn(&page).contains(&"Copied once".to_owned()),
            "{:?}",
            drawn(&page)
        );
    }

    #[test]
    fn a_snippet_with_variables_is_filled_in_before_it_is_copied() {
        let snippets = Snippets::new();
        let restart = snippets.add("Restart deployment", "Ops");
        snippets.write_body(
            &restart,
            "kubectl -n ${namespace} rollout restart deployment/${deployment}",
        );
        let fill = returned_ref(snippets.call(
            "begin_fill",
            &[
                ("lib", Value::resource(&snippets.lib)),
                ("snippet", Value::resource(&restart)),
            ],
        ));
        let values: Vec<ResourceRef> = match snippets.rt.read(&fill, "values").unwrap() {
            Value::List(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::Ref(r) => Some(ResourceRef::clone(&r.target)),
                    _ => None,
                })
                .collect(),
            other => panic!("not values: {other:?}"),
        };
        assert_eq!(
            values
                .iter()
                .map(|value| match snippets.field(value, "name") {
                    Json::String(name) => name,
                    other => panic!("a name is {other:?}"),
                })
                .collect::<Vec<_>>(),
            ["namespace", "deployment"]
        );

        // The sheet's fields are the values, typed into one at a time.
        let mut sheet = snippets.window(
            "fill_sheet",
            vec![Value::resource(&snippets.lib), Value::resource(&fill)],
        );
        let fields: Vec<String> = of_kind(sheet.tree(), "ui.TextInput")
            .into_iter()
            .map(|field| field.key.clone())
            .collect();
        assert_eq!(fields.len(), 2, "{}", dump(sheet.tree()));
        sheet.edit(&fields[0], &Value::text("staging")).unwrap();
        sheet.edit(&fields[1], &Value::text("api")).unwrap();
        // The sheet's main answer copies what was filled in.
        let dialog = of_kind(sheet.tree(), "ui.ModalDialog")
            .into_iter()
            .next()
            .map(|dialog| dialog.key.clone())
            .unwrap_or_else(|| panic!("no sheet in {}", dump(sheet.tree())));
        sheet.fire(&dialog, "act_primary").unwrap();
        assert_eq!(
            snippets.copied(),
            "kubectl -n staging rollout restart deployment/api",
            "what was typed is what is copied"
        );
        assert_eq!(snippets.field(&restart, "uses"), json!(1));
        assert_eq!(
            snippets.field(&fill, "snippet"),
            Json::Null,
            "the filling is put away once it is copied"
        );
    }

    #[test]
    fn a_folder_and_a_tag_are_renamed_everywhere_they_are_used() {
        let snippets = Snippets::new();
        let one = snippets.add("Restart deployment", "Ops");
        let two = snippets.add("Tail logs", "Ops");
        for snippet in [&one, &two] {
            snippets.call(
                "set_tags",
                &[
                    ("snippet", Value::resource(snippet)),
                    ("tags", Value::text("k8s")),
                ],
            );
        }
        snippets.call(
            "rename_folder",
            &[
                ("lib", Value::resource(&snippets.lib)),
                ("folder", Value::text("Ops")),
                ("to", Value::text("Kubernetes")),
            ],
        );
        snippets.call(
            "rename_tag",
            &[
                ("lib", Value::resource(&snippets.lib)),
                ("tag", Value::text("k8s")),
                ("to", Value::text("cluster")),
            ],
        );
        assert_eq!(snippets.field(&one, "folder"), json!("Kubernetes"));
        assert_eq!(snippets.field(&two, "tags"), json!({"count": 1}));

        snippets.call(
            "remove_tag",
            &[
                ("lib", Value::resource(&snippets.lib)),
                ("tag", Value::text("cluster")),
            ],
        );
        assert_eq!(snippets.field(&one, "tags"), json!({"count": 0}));
        // A tag nobody carries cannot be renamed: the action refuses.
        assert!(
            snippets
                .rt
                .invoke_action(
                    &me(),
                    (
                        snippets.app.action("remove_tag").as_str(),
                        snippets.app.args(
                            &snippets.rt,
                            "remove_tag",
                            &[
                                ("lib", Value::resource(&snippets.lib)),
                                ("tag", Value::text("cluster")),
                            ]
                        ),
                    ),
                )
                .is_err(),
            "there is no such tag"
        );
    }

    #[test]
    fn duplicating_puts_the_copy_right_after_and_deleting_takes_it_away() {
        let snippets = Snippets::new();
        let one = snippets.add("Restart deployment", "Ops");
        let two = snippets.add("Tail logs", "Ops");
        let copy = returned_ref(snippets.call(
            "duplicate",
            &[
                ("lib", Value::resource(&snippets.lib)),
                ("snippet", Value::resource(&one)),
            ],
        ));
        assert_eq!(snippets.field(&copy, "title"), json!("Restart deployment"));
        assert_eq!(
            snippets.field(&copy, "uses"),
            json!(0),
            "a copy starts unused"
        );
        let window = snippets.snippets_window();
        let rows: Vec<ResourceRef> = of_kind(window.tree(), "ui.ListItem")
            .into_iter()
            .filter_map(|row| match row.props.get("item") {
                Some(Value::Ref(r)) => Some(ResourceRef::clone(&r.target)),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows.iter().map(ResourceRef::id_string).collect::<Vec<_>>(),
            [one.id_string(), copy.id_string(), two.id_string()],
            "the copy stands right after what it copies"
        );
        snippets.call(
            "delete",
            &[
                ("lib", Value::resource(&snippets.lib)),
                ("snippet", Value::resource(&copy)),
            ],
        );
        assert!(snippets.listed("Tail logs").id_string() == two.id_string());
        assert_eq!(
            snippets.field(&snippets.lib, "snippets"),
            json!({"count": 2})
        );
    }
}
