//! The Sheets app (`apps/tl/sheets`) end to end: a workbook whose formulas are computed, cells
//! filled down and right, charts added and edited, comments answered and their replies taken
//! away, and the window a person works in.

#[cfg(test)]
mod cases {
    use std::collections::BTreeMap;

    use crate::common::*;
    use tilolang_runtime::rich::RichValue;
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const SOURCE: &str = concat!(
        include_str!("../../../../apps/tl/sheets/app.tl"),
        "\n",
        include_str!("../../../../apps/tl/sheets/views.tl"),
    );

    /// The workspace package, as far as this app needs it: its `use` of the people a workspace
    /// has. A stand-in carries the name the import names.
    const WORKSPACE: &str = r#"app tilo.workspace "Workspace"

/// One person of the workspace.
type Member {
    name: Text
}

/// The workspace's members, as its apps read them.
type Members {
    people: Collection<Member>
}

root members: Members

default view members_page for Members

row view member_row for Member

view members_page(m: Members) {
    ui.Text(m.people.len().to_text())
}

view member_row(p: Member) {
    ui.ListRow(title: p.name)
}
"#;

    struct Sheets {
        rt: Runtime,
        app: App,
        book: ResourceRef,
    }

    impl Sheets {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_clock(|| "2026-11-24T09:00:00Z".to_owned());
            let workspace = App::compile(WORKSPACE, &[]);
            workspace.load(&rt);
            let app = App::compile(SOURCE, &[workspace.meta().clone()]);
            app.load(&rt);
            let book = rt.create_root("Workbook").unwrap();
            let sheets = Self { rt, app, book };
            sheets.one("start", vec![Value::resource(&sheets.book)]);
            sheets
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn one(&self, action: &str, args: Vec<Value>) {
            self.call(action, args);
        }

        fn words(&self, text: &str) -> Value {
            Value::Rich(RichValue::from_text(text))
        }

        /// The chart kind `name` (`ChartKind.column` or `ChartKind.line`), as a value.
        fn kind(&self, name: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids.get("ChartKind").unwrap().to_owned(),
                variant: ids.get(name).unwrap().to_owned(),
                fields: BTreeMap::new(),
            })
        }

        /// The workbook's first sheet, as the resource its actions take.
        fn sheet(&self) -> ResourceRef {
            let sheets = self.rt.read(&self.book, "sheets").unwrap();
            let projected = self.rt.project(&sheets);
            let id = projected["items"][0]["id"].as_str().unwrap();
            let ty = self
                .app
                .meta()
                .types
                .iter()
                .find(|t| t.name == "Sheet")
                .unwrap()
                .id
                .clone();
            ResourceRef::parse_id(id, ty).unwrap()
        }

        fn write(&self, cells: &[(&str, &str)]) {
            let refs: Vec<Value> = cells
                .iter()
                .map(|(reference, _)| Value::text(*reference))
                .collect();
            let inputs: Vec<Value> = cells.iter().map(|(_, input)| Value::text(*input)).collect();
            self.one(
                "write_cells",
                vec![
                    Value::resource(&self.book),
                    Value::text("Sheet1"),
                    Value::List(refs),
                    Value::List(inputs),
                ],
            );
        }

        fn read(&self, range: &str) -> Vec<String> {
            self.read_with(range, false)
        }

        /// What the cells in `range` read, as their inputs when `formulas`.
        fn read_with(&self, range: &str, formulas: bool) -> Vec<String> {
            let mut args = vec![
                Value::resource(&self.book),
                Value::text("Sheet1"),
                Value::text(range),
            ];
            if formulas {
                args.push(Value::Bool(true));
            }
            let outcome = self.call("read_cells", args);
            let InvokeOutcome::Done(Value::List(items)) = outcome else {
                panic!("read_cells answers a list: {outcome:?}");
            };
            items
                .iter()
                .map(|item| match item {
                    Value::Text(text) => text.clone(),
                    // The action answers each cell with its reference and what it reads.
                    Value::Tuple(pair) => match pair.last() {
                        Some(Value::Text(text)) => text.clone(),
                        other => panic!("a cell reads {other:?}"),
                    },
                    other => panic!("a cell reads {other:?}"),
                })
                .collect()
        }

        fn cell(&self, reference: &str) -> String {
            match self.rt.read(&self.sheet(), "cells").unwrap() {
                Value::List(entries) => entries
                    .into_iter()
                    .find_map(|entry| match entry {
                        Value::Tuple(pair) => match (pair.first(), pair.get(1)) {
                            (Some(Value::Text(key)), Some(value)) if key == reference => {
                                match value {
                                    Value::Text(text) => Some(text.clone()),
                                    other => panic!("{reference} reads {other:?}"),
                                }
                            }
                            _ => None,
                        },
                        other => panic!("a cell reads {other:?}"),
                    })
                    .unwrap_or_default(),
                other => panic!("cells read {other:?}"),
            }
        }

        fn count(&self, at: &ResourceRef, field: &str) -> usize {
            let held = self.rt.read(at, field).unwrap();
            self.rt.project(&held)["count"].as_u64().unwrap_or(0) as usize
        }
    }

    #[test]
    fn formulas_are_computed_across_the_cells_they_name() {
        let sheets = Sheets::new();
        sheets.write(&[("A1", "2"), ("A2", "3"), ("A3", "=SUM(A1:A2)")]);
        assert_eq!(sheets.read("A1:A3"), ["2", "3", "5"]);
        assert_eq!(sheets.cell("A3"), "=SUM(A1:A2)", "the formula is kept");
    }

    #[test]
    fn filling_down_copies_the_first_row_under_it() {
        let sheets = Sheets::new();
        sheets.write(&[("A1", "10"), ("B1", "20")]);
        sheets.one(
            "fill_down",
            vec![
                Value::resource(&sheets.book),
                Value::resource(&sheets.sheet()),
                Value::text("A1:B2"),
            ],
        );
        assert_eq!(sheets.cell("A2"), "10");
        assert_eq!(sheets.cell("B2"), "20");
    }

    #[test]
    fn filling_right_copies_the_first_column_beside_it() {
        let sheets = Sheets::new();
        sheets.write(&[("A1", "1"), ("A2", "2")]);
        sheets.one(
            "fill_right",
            vec![
                Value::resource(&sheets.book),
                Value::resource(&sheets.sheet()),
                Value::text("A1:C2"),
            ],
        );
        assert_eq!(sheets.cell("B1"), "1");
        assert_eq!(sheets.cell("C1"), "1");
        assert_eq!(sheets.cell("B2"), "2");
    }

    #[test]
    fn a_chart_is_added_edited_and_deleted() {
        let sheets = Sheets::new();
        sheets.write(&[("A1", "1"), ("A2", "4")]);
        let chart = returned_ref(sheets.call(
            "add_chart",
            vec![
                Value::resource(&sheets.sheet()),
                Value::text("A1:A2"),
                sheets.kind("ChartKind.column"),
                Value::text("Sales"),
            ],
        ));
        assert_eq!(sheets.count(&sheets.sheet(), "charts"), 1);

        let kind = sheets.kind("ChartKind.line");
        sheets.one(
            "edit_chart",
            vec![
                Value::resource(&chart),
                Value::text("Revenue"),
                Value::Opt(Some(Box::new(kind))),
                Value::text("A1:A2"),
            ],
        );
        match sheets.rt.read(&chart, "title").unwrap() {
            Value::Text(title) => assert_eq!(title, "Revenue"),
            other => panic!("the chart's title reads {other:?}"),
        }

        sheets.one(
            "delete_chart",
            vec![Value::resource(&sheets.sheet()), Value::resource(&chart)],
        );
        assert_eq!(sheets.count(&sheets.sheet(), "charts"), 0);
    }

    #[test]
    fn a_comment_is_answered_and_its_reply_is_taken_away() {
        let sheets = Sheets::new();
        let comment = returned_ref(sheets.call(
            "comment",
            vec![
                Value::resource(&sheets.book),
                Value::text("Sheet1"),
                Value::text("A1"),
                sheets.words("Is this the total?"),
            ],
        ));
        let reply = returned_ref(sheets.call(
            "reply",
            vec![
                Value::resource(&comment),
                sheets.words("Yes, for the quarter."),
            ],
        ));
        assert_eq!(sheets.count(&comment, "replies"), 1);

        sheets.one(
            "delete_reply",
            vec![Value::resource(&comment), Value::resource(&reply)],
        );
        assert_eq!(sheets.count(&comment, "replies"), 0);

        sheets.one(
            "resolve",
            vec![Value::resource(&comment), Value::Bool(true)],
        );
        match sheets.rt.read(&comment, "resolved").unwrap() {
            Value::Bool(resolved) => assert!(resolved),
            other => panic!("a comment's state reads {other:?}"),
        }
    }

    #[test]
    fn a_cell_shows_a_number_as_yes_or_no_when_it_is_formatted_so() {
        let sheets = Sheets::new();
        sheets.write(&[("A1", "1"), ("A2", "0")]);
        assert_eq!(sheets.read("A1:A2"), ["1", "0"], "as the numbers they are");
        sheets.one(
            "format_cells",
            vec![
                Value::resource(&sheets.book),
                Value::resource(&sheets.sheet()),
                Value::text("A1:A2"),
                Value::text("boolean"),
            ],
        );
        assert_eq!(
            sheets.read("A1:A2"),
            ["TRUE", "FALSE"],
            "the format is the cell's, and the value behind it does not change"
        );
        assert_eq!(sheets.read_with("A1:A2", true), ["1", "0"]);
    }

    #[test]
    fn the_window_opens_a_reference_entry_from_anywhere() {
        let sheets = Sheets::new();
        let mut window = sheets
            .rt
            .open_view(
                &me(),
                &sheets.app.view("workbook"),
                vec![Value::resource(&sheets.book)],
            )
            .unwrap();
        let bar_of = |window: &tilolang_runtime::ViewInstance| {
            let tree = window.tree().clone();
            let shell = of_kind(&tree, "ui.NavShell")
                .into_iter()
                .next()
                .expect("the workbook's window shell");
            shell
                .clone()
                .children
                .iter()
                .find(|child| {
                    child.kind == "part" && child.props.get("name") == Some(&Value::text("actions"))
                })
                .unwrap_or_else(|| panic!("the window's bar:\n{}", dump(&tree)))
                .clone()
        };
        let bar = bar_of(&window);
        assert!(
            of_kind(&bar, "ui.TextInput")
                .iter()
                .all(|field| !field.props.contains_key("autofocus")),
            "the bar opens with the name box standing as it is:\n{}",
            dump(&bar)
        );

        // The Format menu's Go to a cell, as the window's Ctrl+G presses it.
        let go = of_kind(&bar, "ui.MenuItem")
            .into_iter()
            .find(|item| item.props.get("label") == Some(&Value::text("Go to a cell…")))
            .unwrap_or_else(|| panic!("the window offers Go to a cell:\n{}", dump(&bar)))
            .key
            .clone();
        window.fire(&go, "choose").unwrap();
        window.poll_patches().unwrap();
        let entry = of_kind(&bar_of(&window), "ui.TextInput")
            .into_iter()
            .find(|field| field.props.get("autofocus") == Some(&Value::Int(1)))
            .unwrap_or_else(|| panic!("the entry takes the keyboard:\n{}", dump(&bar)))
            .key
            .clone();
        window
            .fire_with(&entry, "submit", Some(&Value::text("c7")))
            .unwrap();
        window.poll_patches().unwrap();
        assert!(
            of_kind(&bar_of(&window), "ui.TextInput")
                .iter()
                .all(|field| !field.props.contains_key("autofocus")),
            "the entry closes once the reference is given:\n{}",
            dump(&bar_of(&window))
        );
        let grid = of_kind(window.tree(), "ui.Sheet")
            .into_iter()
            .next()
            .expect("the grid");
        let chosen = match grid.props.get("selection") {
            Some(Value::Text(at)) => at.clone(),
            Some(Value::Opt(Some(held))) => match &**held {
                Value::Text(at) => at.clone(),
                other => panic!("the grid's selection reads {other:?}"),
            },
            other => panic!("the grid's selection reads {other:?}"),
        };
        assert_eq!(
            chosen, "C7",
            "the grid stands on the reference the person typed"
        );
    }

    #[test]
    fn the_window_shows_the_grid_the_bar_and_the_tabs() {
        let sheets = Sheets::new();
        let window = sheets
            .rt
            .open_view(
                &me(),
                &sheets.app.view("workbook"),
                vec![Value::resource(&sheets.book)],
            )
            .unwrap();
        let tree = window.tree();
        let shown = crate::common::dump(tree);
        for expected in ["Sheet1", "Add sheet"] {
            assert!(shown.contains(expected), "no {expected} in\n{shown}");
        }
        // The workbook's bar is the page's header: it stands in the window's bar.
        let shell = of_kind(tree, "ui.NavShell")
            .into_iter()
            .next()
            .expect("the workbook's window shell");
        let bar = shell
            .children
            .iter()
            .find(|child| {
                child.kind == "part" && child.props.get("name") == Some(&Value::text("actions"))
            })
            .unwrap_or_else(|| panic!("the window's bar holds the workbook's bar:\n{shown}"));
        let bar_shown = crate::common::dump(bar);
        for expected in ["AutoSum", "Value or formula"] {
            assert!(
                bar_shown.contains(expected),
                "no {expected} in the workbook's bar:\n{bar_shown}"
            );
        }
    }
}
