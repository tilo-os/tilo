//! The snippets app (`apps/tl/snippets/app.tl`) end to end: making, starring, filing and tagging
//! snippets, copying one, filling in its variables before copying, renaming folders and tags, and
//! the window's places, rows and page.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use serde_json::{Value as Json, json};
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value, ViewInstance};

    const SOURCE: &str = include_str!("../../../../apps/tl/snippets/app.tl");

    struct Snippets {
        rt: Runtime,
        app: App,
        lib: ResourceRef,
        selection: Clipboard,
    }

    impl Snippets {
        fn new() -> Self {
            let authority = TestAuthority::new();
            let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
            let selection = Clipboard::default();
            rt.set_clipboard(selection.clone());
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let app = App::compile(SOURCE, &[]);
            authority.trust(&app.load(&rt));
            // Copying reaches the person's clipboard, outside Tilo, which the policy grants the
            // app it is written in.
            for action in ["copy", "copy_filled", "copy_text"] {
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
            let page = self.window("snippet_page", vec![Value::resource(snippet)]);
            let editor = of_kind(page.tree(), "ui.CodeEditor")
                .into_iter()
                .next()
                .map_or_else(
                    || panic!("no body editor in {}", dump(page.tree())),
                    |editor| editor.key.clone(),
                );
            page.edit(&editor, &Value::text(text)).unwrap();
        }

        fn copied(&self) -> String {
            assert!(self.selection.held().is_some(), "nothing was copied");
            self.selection.text()
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

    tilo_test_env::scenario! {
        fn e2e_snippets_1;

        phase a_snippet_is_made_starred_filed_and_tagged;
        {
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
                        Some(Value::Int(n)) => f64::from(i32::try_from(*n).unwrap_or(0)),
                        Some(Value::Float(n)) => *n,
                        Some(Value::Opt(Some(held))) => match &**held {
                            Value::Int(n) => f64::from(i32::try_from(*n).unwrap_or(0)),
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

        phase copying_a_snippet_puts_it_on_the_clipboard_and_counts_the_use;
        {
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
            // The window says the copy went through, and that it does not stay.
            let said = snippets.rt.take_said();
            assert_eq!(said.len(), 1, "{said:?}");
            assert_eq!(said[0].text, "Copied");
            assert_eq!(said[0].kind, tilolang_runtime::notice::Kind::Success);
            let page = snippets.window("snippet_page", vec![Value::resource(&restart)]);
            assert!(
                drawn(&page).contains(&"Copied once".to_owned()),
                "{:?}",
                drawn(&page)
            );
        }

        phase a_refused_copy_says_so_and_counts_nothing;
        // A copy the device refuses says so, and the snippet is not counted as used.
        {
            let snippets = Snippets::new();
            let restart = snippets.add("Restart deployment", "Ops");
            snippets.write_body(&restart, "kubectl rollout restart deployment/web");
            snippets.selection.refuse(true);
            snippets.call("copy", &[("snippet", Value::resource(&restart))]);
            assert_eq!(snippets.field(&restart, "uses"), json!(0));
            assert!(snippets.selection.held().is_none());
            let said = snippets.rt.take_said();
            assert_eq!(said.len(), 1, "{said:?}");
            assert_eq!(said[0].text, "Could not copy to the clipboard");
            assert_eq!(said[0].kind, tilolang_runtime::notice::Kind::Error);
        }

        phase a_use_is_recorded_without_copying;
        // A use is counted without a copy: the body never reaches the clipboard and the count moves
        // anyway, which is what a script or an agent that took the body its own way records.
        {
            let snippets = Snippets::new();
            let restart = snippets.add("Restart deployment", "Ops");
            snippets.write_body(&restart, "kubectl rollout restart deployment/web");
            assert_eq!(snippets.field(&restart, "uses"), json!(0));
            snippets.call("record_use", &[("snippet", Value::resource(&restart))]);
            assert_eq!(snippets.field(&restart, "uses"), json!(1));
            assert!(
                snippets.selection.held().is_none(),
                "recording a use put nothing on the clipboard"
            );
            // The page counts the use the same way a copy does.
            let page = snippets.window("snippet_page", vec![Value::resource(&restart)]);
            assert!(
                drawn(&page).contains(&"Copied once".to_owned()),
                "{:?}",
                drawn(&page)
            );
        }

        phase a_snippet_carrying_a_credential_is_copied_as_a_secret_that_leaves_the_clipboard;
        {
            let snippets = Snippets::new();
            let key = snippets.add("Staging key", "Ops");
            snippets.write_body(&key, "export API_KEY=sk-live-4f2a");
            let page = snippets.window("snippet_page", vec![Value::resource(&key)]);
            assert!(
                drawn(&page)
                    .iter()
                    .any(|said| said.contains("leaves the clipboard")),
                "the page says a copy will not stay: {:?}",
                drawn(&page)
            );
            snippets
                .rt
                .invoke_action(
                    &me(),
                    (
                        snippets.app.action("copy").as_str(),
                        vec![Value::resource(&key)],
                    ),
                )
                .unwrap_or_else(|error| panic!("copy: {error:?}"));
            assert_eq!(snippets.copied(), "export API_KEY=sk-live-4f2a");
            assert_eq!(
                snippets.selection.clears_after(),
                Some(tilolang_runtime::clipboard::SECRET_CLEARS_AFTER),
                "the key is a secret copy, taken off the clipboard again"
            );
            let said = snippets.rt.take_said();
            assert_eq!(said.len(), 1, "{said:?}");
            assert_eq!(said[0].text, "Copied; it leaves the clipboard in a minute");

            // A plain snippet is an ordinary copy, kept as any other.
            let note = snippets.add("Greeting", "Writing");
            snippets.write_body(&note, "Thanks for the quick review!");
            snippets
                .rt
                .invoke_action(
                    &me(),
                    (
                        snippets.app.action("copy").as_str(),
                        vec![Value::resource(&note)],
                    ),
                )
                .unwrap_or_else(|error| panic!("copy: {error:?}"));
            assert_eq!(snippets.copied(), "Thanks for the quick review!");
            assert_eq!(snippets.selection.clears_after(), None);
            let said = snippets.rt.take_said();
            assert_eq!(said.len(), 1, "{said:?}");
            assert_eq!(said[0].text, "Copied", "a body that stays says so");
            let page = snippets.window("snippet_page", vec![Value::resource(&note)]);
            assert!(
                !drawn(&page)
                    .iter()
                    .any(|said| said.contains("leaves the clipboard")),
                "{:?}",
                drawn(&page)
            );
        }

        phase a_snippet_with_variables_is_filled_in_before_it_is_copied;
        {
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
            let sheet = snippets.window(
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
                .map_or_else(
                    || panic!("no sheet in {}", dump(sheet.tree())),
                    |dialog| dialog.key.clone(),
                );
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

        phase a_folder_and_a_tag_are_renamed_everywhere_they_are_used;
        {
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

        phase duplicating_puts_the_copy_right_after_and_deleting_takes_it_away;
        {
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
            assert_eq!(
                snippets.field(&copy, "title"),
                json!("Restart deployment (copy)"),
                "a copy is named as a copy of what it copies"
            );
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
            assert_eq!(snippets.listed("Tail logs").id_string(), two.id_string());
            assert_eq!(
                snippets.field(&snippets.lib, "snippets"),
                json!({"count": 2})
            );
        }
    }
}
