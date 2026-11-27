tilo_test_env::scenario! {
    fn palette_action_rows_read_as_outcomes_with_picture_and_keys;

    phase listed_action_rows_read_as_outcomes_once_without_ids_glyphs_or_control_verbs;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let new_folder = || {
            ActionSpec::immediate(
                "new-folder",
                "New folder",
                NOTE_TYPE,
                &Note {
                    key: "folder".into(),
                    value: "new".into(),
                },
            )
            .unwrap()
        };
        let mut focus = new_folder();
        focus.type_mut().label = "Focus".into();
        let resources = vec![
            ResourceSpec::new("feeds.library", "feeds.library", "Feeds")
                .with_action(new_folder())
                .with_child(
                    ResourceSpec::new("feeds.library/feeds/1", "feed", "Hacker News").with_action(
                        ActionSpec::immediate(
                            "unsubscribe",
                            "Unsubscribe",
                            NOTE_TYPE,
                            &Note {
                                key: "feed".into(),
                                value: "gone".into(),
                            },
                        )
                        .unwrap(),
                    ),
                ),
            // A bar button verb, as the control projection publishes
            // it: for agents, never a person's row.
            ResourceSpec::new("ui:new-folder", "tilo.ui.control", "New folder")
                .with_action(focus.for_audience(tilo_actions::Audience::Agents)),
            ResourceSpec::new("feeds.library", "feeds.library", "Feeds").with_action(new_folder()),
        ];
        let mut palette = ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let rows: Vec<(String, Option<String>)> = palette
            .results
            .groups()
            .iter()
            .flat_map(|group| &group.results)
            .map(|row| (row.title.clone(), row.subtitle.clone()))
            .collect();
        assert_eq!(
            rows.iter()
                .filter(|(title, _)| title == "New folder")
                .count(),
            1,
            "one row however many parts publish it: {rows:?}"
        );
        assert!(
            rows.contains(&("Unsubscribe".to_owned(), Some("Hacker News".to_owned()))),
            "an item's action names the item under it: {rows:?}"
        );
        for (title, subtitle) in &rows {
            let row = tilo_actions::ListedRow {
                title: title.clone(),
                subtitle: subtitle.clone(),
            };
            assert!(
                tilo_actions::listed_row_problems(&row).is_empty(),
                "{row:?} reads with {:?}",
                tilo_actions::listed_row_problems(&row)
            );
        }
    }

    // An action's row in the window search says what every list says of
    // it — its picture and its declared keys as keycaps — and lists only what
    // the app offers a person: never the host's operations or the window's
    // control mirrors.
    phase action_rows_carry_the_actions_picture_and_its_keys;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut new_task = ActionSpec::immediate(
            "new-task",
            "New task",
            NOTE_TYPE,
            &Note {
                key: "task".into(),
                value: "new".into(),
            },
        )
        .unwrap();
        {
            let ty = new_task.type_mut();
            ty.icon = Some("plus".into());
            ty.state.shortcut = Some("Ctrl+n".into());
        }
        let tapped = |template: &ActionSpec, id: &str, label: &str| {
            let mut spec = template.clone();
            spec.type_mut().id = id.into();
            spec.type_mut().label = label.into();
            spec
        };
        let run_block = tapped(&new_task, "run-block", "Run a block");
        let focus = tapped(&new_task, "focus", "Focus");
        let internal_step = tapped(&new_task, "internal", "Internal step");
        let resources = vec![
            ResourceSpec::new("tasks", "tasks.Tasks", "Tasks").with_action(new_task),
            ResourceSpec::new("tl:host", "tl.host", "tilolang").with_action(run_block),
            ResourceSpec::new("ui:new-task", "tilo.ui.control", "New task").with_action(focus),
            ResourceSpec::new("tasks:internal", "tasks.Task", " ").with_action(internal_step),
        ];
        let mut palette = ActionPalette::new();
        palette.set_platform_conventions(tilo_ui_base::platform::PlatformConventions::new(
            tilo_ui_base::platform::Platform::Linux,
        ));
        palette.load_open_resources(&resources);
        palette.open();
        let rows: Vec<crate::search::SearchResult> = palette
            .results
            .groups()
            .iter()
            .flat_map(|group| group.results.iter().cloned())
            .collect();
        let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, vec!["New task"], "only the app's own action");
        let row = &rows[0];
        assert_eq!(row.icon, Some(tilo_ui_base::icons::Icon::Plus));
        let Some(crate::search::SearchRow::Command(command)) = &row.row else {
            panic!("the action row is painted from the action's own state: {row:?}");
        };
        assert_eq!(
            command.keycaps,
            Some(vec!["Ctrl".to_owned(), "N".to_owned()]),
            "the declared chord as the keys it names"
        );
    }

    phase the_same_action_of_two_documents_is_two_rows_naming_each;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let mark = || {
            ActionSpec::immediate(
                "mark",
                "Mark done",
                NOTE_TYPE,
                &Note {
                    key: "marked".into(),
                    value: "yes".into(),
                },
            )
            .unwrap()
        };
        let resources = vec![
            ResourceSpec::new("a", "thing", "Alpha").with_action(mark()),
            ResourceSpec::new("b", "thing", "Beta").with_action(mark()),
        ];
        let mut palette = ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let mut rows: Vec<(String, Option<String>)> = palette
            .results
            .groups()
            .iter()
            .flat_map(|group| &group.results)
            .map(|row| (row.title.clone(), row.subtitle.clone()))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("Mark done".to_owned(), Some("Alpha".to_owned())),
                ("Mark done".to_owned(), Some("Beta".to_owned())),
            ]
        );
    }
}
