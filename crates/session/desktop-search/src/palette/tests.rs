use super::*;
use crate::model::LaunchPlace;

mod early_enter;
use ui::search::AppScope;
use ui::shortcut_table::{ShortcutAction, ShortcutConfig};
use ui::winit::keyboard::ModifiersState;

pub(super) fn data() -> SearchData {
    SearchData {
        applications: vec![
            AppEntry {
                id: "so.tilo.sheets".into(),
                title: "Sheets".into(),
                category: Some("Office".into()),
            },
            AppEntry {
                id: "so.tilo.notes".into(),
                title: "Notes".into(),
                category: None,
            },
        ],
        windows: vec![WindowEntry {
            tab: 3,
            title: "Notes".into(),
            app_title: "Notes".into(),
            app: Some("so.tilo.notes".into()),
            place: vec!["Workspace".into(), "Main".into()],
        }],
        scope: Scope::Workspace,
        ..SearchData::default()
    }
}

tilo_test_env::scenario! {
    fn the_palette_lists_what_is_now_and_what_kinds_sources_and_scope_leave;

    phase the_empty_card_names_its_groups;
    // Opened before anything is typed, the card names each group it holds:
    // "Apps" over the applications, "Windows" over the open windows. A
    // person never reads a bare list, and the field says what those groups
    // are ("Search apps, windows, documents and actions…").
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        assert_eq!(search.query(), "", "nothing typed yet");
        let groups: Vec<&str> = search
            .palette()
            .visible_entries()
            .iter()
            .map(|entry| {
                entry
                    .group
                    .as_deref()
                    .expect("every row is under a named group")
            })
            .collect();
        assert!(groups.contains(&"Apps"), "{groups:?}");
        assert!(groups.contains(&"Windows"), "{groups:?}");
        assert_eq!(
            crate::i18n::t("search-placeholder"),
            "Search apps, windows, documents and actions\u{2026}",
            "the empty field names those groups"
        );
    }

    phase the_empty_palette_lists_what_is_now_before_the_ways_into_everything;
    // With nothing typed the palette lists what is useful now, in that
    // order: who is working here, what the person chose last, then the
    // apps and windows it opens. What is left of the workspace (leaving it
    // on this computer) is something a person goes looking for, never the
    // default list.
    {
        use crate::providers::happening::{Happening, PersonHere, WorkspaceHere};
        let here = WorkspaceHere {
            workspace: "workspace".into(),
            name: "Studio".into(),
            active: true,
            people: vec![PersonHere {
                id: "ada".into(),
                name: "Ada".into(),
                colour: (200, 100, 50),
                agent: false,
                away: false,
                tab: None,
            }],
            windows: Vec::new(),
            calls: Vec::new(),
        };
        let mut data = data();
        data.commands.push(CommandSpec {
            id: "session-host-leave-workspace".into(),
            title: "Leave workspace".into(),
            subtitle: None,
            icon: Some(ui::icons::Icon::ArrowUpRight),
            unasked: false,
        });
        data.sources
            .push(Held::new(Happening::new(false, vec![here])));
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        // The row the person chose last stands in Recent.
        let sheets = search
            .results()
            .iter()
            .find(|hit| hit.title == "Sheets")
            .cloned()
            .expect("the app is listed");
        Arc::make_mut(&mut search.recents).record(&sheets);
        search.restart("", ui::search::engine_generation());
        settled(&mut search);

        let entries = search.palette().visible_entries();
        let mut groups: Vec<&str> = Vec::new();
        for entry in &entries {
            if let Some(group) = entry.group.as_deref()
                && groups.last() != Some(&group)
            {
                groups.push(group);
            }
        }
        assert_eq!(
            groups,
            ["Happening now", "Recent", "Apps", "Windows"]
        );
        assert!(
            entries.iter().all(|entry| entry.label != "Leave workspace"),
            "no workspace housekeeping is a default: {entries:?}"
        );
    }

    phase the_empty_palette_continues_where_the_person_was;
    // Nothing typed starts where the person was: the place their window is
    // in leads the card, above the apps and the windows, and no budget cuts
    // it away however crowded the card gets.
    {
        use crate::providers::places::{Place, RecentPlaces};
        let mut recent = RecentPlaces::default();
        recent.record(Place {
            app: "so.tilo.file-explorer".into(),
            workspace: "workspace".into(),
            tab: 9,
            label: "Home".into(),
            at: 3,
            device: None,
        });
        let mut data = data();
        data.sources.push(Held::new(continue_source(&recent)));
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert_eq!(
            entries.first().map(|entry| entry.label.as_str()),
            Some("Home"),
            "the place leads the empty card: {entries:?}"
        );
        assert_eq!(entries[0].group.as_deref(), Some("Continue"));
        // A card too small for everything it has makes way with what is
        // least likely wanted then, never with where the person was.
        search.set_room(400.0, 200.0);
        search.restart("", ui::search::engine_generation());
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert!(
            entries.iter().any(|entry| entry.id == ALL_RESULTS_ID),
            "the small card leaves a group to the all-results row: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.label == "Home" && entry.group.as_deref() == Some("Continue")),
            "the place stays on a crowded card: {entries:?}"
        );
    }

    phase every_app_is_one_row_under_the_apps;
    // Under the apps the empty palette offers every one of them: the
    // catalogue, as the empty shell's launchpad opens it.
    {
        let mut full = data();
        // More apps than the card lists unasked, as a full fleet has.
        full.applications.extend((0..40).map(|i| AppEntry {
            id: format!("so.tilo.app{i}"),
            title: format!("App {i}"),
            category: None,
        }));
        let mut search = DesktopSearch::new();
        search.open(full);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let last = entries
            .iter()
            .rfind(|entry| entry.group.as_deref() == Some("Apps"))
            .expect("the apps' group");
        assert_eq!(last.id, ALL_APPS_ID);
        assert_eq!(last.label, "All apps");
        assert!(
            entries
                .iter()
                .filter(|entry| entry.group.as_deref() == Some("Apps"))
                .all(|entry| !entry.id.starts_with(MORE_PREFIX)),
            "one way to every app, not a \"Show all\" beside it: {entries:?}"
        );
        let at = entries
            .iter()
            .position(|entry| entry.id == ALL_APPS_ID)
            .unwrap();
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing it closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Command {
                id: crate::model::ALL_APPS_COMMAND.into()
            })
        );

        // The catalogue itself is every app: it does not offer itself again.
        let mut catalogue = data();
        catalogue.scope = Scope::WindowGroup(None);
        catalogue.windows.clear();
        catalogue.commands.clear();
        catalogue.resources.clear();
        catalogue.sources.clear();
        let mut search = DesktopSearch::new();
        search.open(catalogue);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert!(
            entries.iter().any(|entry| entry.label == "Sheets"),
            "the catalogue lists the apps: {entries:?}"
        );
        assert!(
            entries.iter().all(|entry| entry.id != ALL_APPS_ID),
            "and offers no second way into itself"
        );
    }

    phase the_kind_chips_narrow_the_rows_to_the_one_picked;
    // The card offers a chip per kind it found, and picking one leaves the
    // rows of that kind: the window rows go while "Apps" is picked, and the
    // chip stays offered so the person can widen them again.
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        let chips: Vec<(String, String)> = search
            .palette()
            .facets()
            .iter()
            .flat_map(|facet| {
                facet
                    .values
                    .iter()
                    .map(|value| (value.value.clone(), value.label.clone()))
            })
            .collect();
        assert!(
            chips.contains(&("Apps".to_owned(), "Apps".to_owned())),
            "every app is under the Apps chip: {chips:?}"
        );
        assert!(
            chips.contains(&("Windows".to_owned(), "Windows".to_owned())),
            "the open window is under the Windows chip: {chips:?}"
        );
        let kinds = |search: &DesktopSearch| -> Vec<String> {
            search
                .palette()
                .visible_entries()
                .iter()
                .filter_map(|entry| entry.group.clone())
                .collect()
        };
        assert!(kinds(&search).iter().any(|group| group == "Windows"));
        search
            .palette_mut()
            .toggle_facet(&ui::search::FacetFilter::new("kind", "Apps"));
        settled(&mut search);
        let shown = kinds(&search);
        assert!(!shown.is_empty(), "the apps stay: {shown:?}");
        assert!(
            shown.iter().all(|group| group == "Apps"),
            "picking Apps leaves only its rows: {shown:?}"
        );
        assert!(
            search
                .palette()
                .facets()
                .iter()
                .any(|facet| facet.values.iter().any(|value| value.value == "Windows")),
            "the chip that widens the rows again stays offered"
        );
        search
            .palette_mut()
            .toggle_facet(&ui::search::FacetFilter::new("kind", "Apps"));
        settled(&mut search);
        assert!(
            kinds(&search).iter().any(|group| group == "Windows"),
            "unpicking the chip brings every kind back"
        );
    }

    phase switching_a_source_off_from_its_row_takes_its_rows_away_and_stays_open;
    {
        let mut search = DesktopSearch::new();
        search.open(with_notes(Scope::Workspace));
        settled(&mut search);
        search.set_query("groceries tomatoes");
        settled(&mut search);
        assert!(
            search.results().iter().any(|hit| hit.provider == "notes"),
            "{:?}",
            search.results()
        );

        search.set_query("disable notes");
        settled(&mut search);
        let at = search
            .palette()
            .visible_entries()
            .iter()
            .position(|entry| entry.label == "Disable Notes search")
            .expect("the source's switch is offered");
        let entries = search.palette().visible_entries();
        assert_eq!(entries[at].badge.as_deref(), Some("ON"));
        drop(entries);
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();

        assert!(
            !search.observe(),
            "a switch keeps search open on what it switched"
        );
        assert!(search.is_open());
        assert_eq!(
            search.take_choice(),
            Some(Target::Prefer {
                source: "notes".into(),
                enabled: false
            }),
            "the host keeps the person's choice"
        );

        // The switch now offers to switch it back on...
        let switched = |search: &DesktopSearch| {
            search
                .results()
                .iter()
                .find(|hit| matches!(hit.target, Target::Prefer { .. }))
                .map(|hit| hit.title.clone())
        };
        settled(&mut search);
        assert_eq!(switched(&search).as_deref(), Some("Enable Notes search"));
        // ...and what it switched off is gone.
        search.set_query("groceries tomatoes");
        settled(&mut search);
        assert!(
            search.results().iter().all(|hit| hit.provider != "notes"),
            "{:?}",
            search.results()
        );
        // The desktop's own rows stay, whatever was switched: the app.
        search.set_query("sheets");
        settled(&mut search);
        assert!(search.results().iter().any(|hit| hit.title == "Sheets"));
    }

    phase the_actions_stay_whatever_is_switched_off;
    {
        let mut search = DesktopSearch::new();
        search.open(with_notes(Scope::Workspace));
        settled(&mut search);
        search.set_query("disable");
        settled(&mut search);
        assert!(
            !search.results().iter().any(|hit| matches!(
                &hit.target,
                Target::Prefer { source, .. } if source == "actions"
            )),
            "the actions are how anything is done from search: {:?}",
            search.results()
        );
    }

    phase a_search_over_the_workspace_in_front_offers_to_widen_it;
    {
        let mut search = DesktopSearch::new();
        search.open(with_notes(Scope::Workspace));
        settled(&mut search);
        assert_eq!(search.palette().scope_token(), Some("Workspace"));
        assert_eq!(
            search.palette().scope_token_action(),
            Some("Search every workspace")
        );

        // Widened, the token is gone: there is nothing left to widen.
        search.close();
        search.open(with_notes(Scope::Everywhere));
        settled(&mut search);
        assert_eq!(search.palette().scope_token(), None);
        assert_eq!(search.palette().scope_token_action(), None);
    }

    phase a_search_of_the_desktops_own_names_no_scope;
    // A search the desktop opens of its own (the apps a new tab may show)
    // is not narrowed to a workspace, and says nothing about one.
    {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            applications: data().applications,
            scope: Scope::WindowGroup(None),
            ..SearchData::default()
        });
        settled(&mut search);
        assert_eq!(search.palette().scope_token(), None);
        assert_eq!(search.palette().scope_token_action(), None);
    }

    phase a_chosen_row_leads_the_next_empty_palette;
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        search.palette_mut().activate_selected();
        search.observe();
        search.open(data());
        settled(&mut search);
        assert_eq!(search.results()[0].provider, "recent");
        assert_eq!(search.results()[0].title, "Sheets");
    }

    phase a_windows_search_lists_its_resources_then_its_actions;
    // A window's search lists what the window holds, then what can be
    // done in it, before anything is typed: the actions are part of it.
    {
        let action = |id: &str, label: &str| {
            tilo_actions::ActionSpec::immediate_encoded(
                id,
                label,
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.action".into(),
                    payload: Vec::new(),
                },
            )
        };
        // The framework's `open`, which shows the item at a hit's anchor.
        let open = tilo_actions::ActionSpec::typed_operation(
            "open",
            "Open",
            "Item to open",
            tilo_actions::OperationKind::View,
            tilo_actions::remote::APPLICATION_OPEN_TYPE,
            vec![tilo_actions::ActionParam::required(
                "anchor",
                tilo_actions::ParamType::Text,
                "Item to open",
            )],
            |_| tilo_actions::EncodedAction {
                type_id: tilo_actions::remote::APPLICATION_OPEN_TYPE.into(),
                payload: Vec::new(),
            },
        );
        let mut notebook = ResourceSpec::new("notes.notebook", "notes.notebook", "Notebook")
            .with_action(open)
            .with_action(action("new-note", "New note"));
        notebook.children = vec![ResourceSpec::new("page:1", "notes.page", "Groceries")];
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![notebook],
            providers: vec![Box::new(Places)],
            scope: Scope::InApp(AppScope::front()),
            ..SearchData::default()
        });
        settled(&mut search);
        let rows: Vec<(&str, &ResourceKind)> = search
            .results()
            .iter()
            .map(|hit| (hit.title.as_str(), &hit.kind))
            .collect();
        assert_eq!(
            rows,
            [
                ("Home", &ResourceKind::Custom("Places".into())),
                ("New note", &ResourceKind::Action),
            ]
        );
    }

    phase the_keyboard_reaches_every_app_from_an_empty_palette;
    // From an empty palette the keyboard reaches every app: the apps listed, then "All apps",
    // the one row that leads to the rest, which lists every app one arrow press apart.
    {
        let data = crowded();
        let total = data.applications.len();
        assert!(total > 20, "more apps than one screen: {total}");
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        let header = search
            .palette()
            .visible_entries()
            .iter()
            .find(|entry| entry.group.as_deref() == Some("Apps"))
            .and_then(|entry| entry.group_total);
        assert_eq!(header, Some(total), "the header counts every app");

        let mut on_all_apps = false;
        for _ in 0..(total * 4) {
            let id = search.palette().selected_id().map(str::to_owned);
            assert!(
                !id.as_deref().is_some_and(|id| id.starts_with(MORE_PREFIX)),
                "no second way to every app beside All apps"
            );
            if id.as_deref() == Some(ALL_APPS_ID) {
                on_all_apps = true;
                break;
            }
            if !arrow_down(&mut search) {
                break;
            }
        }
        assert!(on_all_apps, "the keyboard reaches All apps");
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing it closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Command {
                id: crate::model::ALL_APPS_COMMAND.into()
            })
        );

        // What it opens: the apps alone, every one of them a press apart.
        let mut catalogue = crowded();
        catalogue.scope = Scope::WindowGroup(None);
        let mut search = DesktopSearch::new();
        search.open(catalogue);
        settled(&mut search);
        let mut reached = std::collections::BTreeSet::new();
        for _ in 0..(total * 4) {
            let id = search.palette().selected_id().map(str::to_owned);
            if let Some(id) = id {
                if id.starts_with(MORE_PREFIX) {
                    search.palette_mut().activate_selected();
                    assert!(!search.observe(), "showing all keeps search open");
                    continue;
                }
                if let Some(Target::Launch { app, .. }) = search.hit(&id).map(|hit| hit.target) {
                    reached.insert(app);
                }
            }
            if !arrow_down(&mut search) {
                break;
            }
        }
        assert_eq!(reached.len(), total, "every app is one arrow press away");
    }
}

/// Someone is here: the empty palette leads with them and the call
/// they could join, and choosing that row joins it.
#[test]
fn the_empty_palette_leads_with_who_is_here() {
    use crate::providers::happening::{CallHere, Happening, PersonHere, WorkspaceHere};
    let person = PersonHere {
        id: "ada".into(),
        name: "Ada".into(),
        colour: (200, 100, 50),
        agent: false,
        away: false,
        tab: None,
    };
    let here = WorkspaceHere {
        workspace: "workspace".into(),
        name: "Studio".into(),
        active: true,
        people: vec![person.clone()],
        windows: Vec::new(),
        calls: vec![CallHere {
            channel: "c_voice".into(),
            name: String::new(),
            people: vec![person],
        }],
    };
    let mut data = data();
    data.sources
        .push(Held::new(Happening::new(false, vec![here])));
    let mut search = DesktopSearch::new();
    search.open(data);
    settled(&mut search);
    let entries = search.palette().visible_entries();
    assert_eq!(
        entries[0].group.as_deref(),
        Some("Happening now"),
        "who is here leads the empty palette"
    );
    assert_eq!(entries[0].label, "Studio");
    assert_eq!(entries[0].faces.len(), 1, "their face is worn");
    let call = entries
        .iter()
        .find(|entry| entry.label == "Call")
        .expect("the call to join");
    assert_eq!(call.outcome.as_deref(), Some("Join"));
    let at = entries
        .iter()
        .position(|entry| entry.id == call.id)
        .unwrap();
    search.palette_mut().set_cursor(at);
    search.palette_mut().activate_selected();
    assert!(search.observe(), "choosing a row closes search");
    let Some(Target::Invoke(invoke)) = search.take_choice() else {
        panic!("the call is joined by asking Chat");
    };
    assert_eq!(invoke.app, "so.tilo.chat");
    assert_eq!(invoke.address.action.as_str(), "join-call");
}

/// Tasks' one-line "New task", as the session publishes the app's actions: `title` is the
/// entry parameter the typed line fills.
fn captures() -> Vec<PublishedActions> {
    let capture = tilo_actions::ActionSpec::prompted_param(
        "new_task",
        "New task",
        tilo_actions::ActionParam::required("title", tilo_actions::ParamType::Text, "What to do")
            .taking_entry(),
        "Call the landlord fri 10am",
        tilo_actions::ExecutionLocus::ClientLocal,
        |text| tilo_actions::EncodedAction {
            type_id: "test.action".into(),
            payload: text.as_bytes().to_vec(),
        },
    );
    let resource =
        tilo_actions::ResourceSpec::new("tasks", "tl.Tasks.task", "Tasks").with_action(capture);
    vec![PublishedActions::of("so.tilo.lang.tasks", "", &[resource])]
}

tilo_test_env::scenario! {
    fn a_row_is_drawn_as_what_it_stands_for_and_choosing_it_does_what_it_says;

    phase a_capture_chosen_from_the_palette_runs_with_the_line_typed;
    // A person presses the palette key, types what they want done, and Enter: the row is the
    // way to write it down, and choosing it runs it with their words — no action opened, no
    // field to fill in again.
    {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: captures(),
            scope: Scope::Workspace,
            ..data()
        });
        settled(&mut search);
        let line = "Call the landlord fri 10am";
        search.set_query(line);
        settled(&mut search);
        let at = {
            let entries = search.palette().visible_entries();
            let at = entries
                .iter()
                .position(|entry| entry.label == "New task")
                .unwrap_or_else(|| {
                    panic!(
                        "the capture answers the sentence: {:?}",
                        entries.iter().map(|entry| &entry.label).collect::<Vec<_>>()
                    )
                });
            assert!(
                entries[at].argument.is_none(),
                "the row carries the line, so Enter runs it"
            );
            at
        };
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a capture closes search");
        let Some(Target::Invoke(invoke)) = search.take_choice() else {
            panic!("the capture runs the app's own action");
        };
        assert_eq!(invoke.app, "so.tilo.lang.tasks");
        assert_eq!(invoke.address.action.as_str(), "new_task");
        assert_eq!(
            invoke.arguments.get("title"),
            Some(&serde_json::Value::String(line.to_owned())),
            "the task is made from the words the person typed"
        );
    }

    phase a_hit_is_drawn_as_the_row_its_app_gave_it;
    {
        let mut message = ResourceSpec::new("m1", "mail.message", "Invoice");
        message
            .actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "open",
                "Open",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.open".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![message],
            providers: vec![Box::new(Mail)],
            ..data()
        });
        settled(&mut search);
        search.set_query("invoice");
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let at = entries
            .iter()
            .position(|entry| entry.label == "Invoice")
            .unwrap_or_else(|| panic!("the mail is found: {entries:?}"));
        assert_eq!(
            entries[at].row,
            Some(
                ui::search::SearchRow::message("Ada", "Invoice")
                    .snippet("Attached is March")
                    .unread(true)
            ),
            "the app's own row reaches the palette"
        );
        assert!(
            entries[at].preview.is_some(),
            "and what the mail holds is under the list"
        );
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a row closes search");
        let Some(Target::Action(action)) = search.take_choice() else {
            panic!("the mail opens through its app's open");
        };
        assert_eq!(
            action.address(),
            tilo_actions::ActionAddress::new("m1", "open")
        );
    }

    phase escape_closes_the_search_whatever_was_typed;
    // Escape closes search from wherever the person stands in it: a query
    // still in the field is no reason for the card to stay over what they
    // were doing.
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("notes");
        settled(&mut search);
        assert!(search.escape(), "the shell's search takes Escape");
        assert!(!search.is_open(), "Escape closes search with a query typed");
        assert!(
            !search.escape(),
            "and Escape on a closed search is not the shell's to handle"
        );
        // An empty field closes it the same way.
        search.open(data());
        settled(&mut search);
        assert!(search.escape());
        assert!(!search.is_open(), "Escape on the empty field closes");
    }

    phase enter_on_a_typed_app_name_chooses_its_launch;
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        assert_eq!(search.results()[0].title, "Sheets");
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a row closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Launch {
                app: "so.tilo.sheets".into(),
                place: LaunchPlace::Focus,
            })
        );
        assert!(!search.is_open());
    }

    phase a_modifier_held_over_a_row_places_the_window_it_names;
    // An application's row says what a modifier held over it does with the
    // window, and choosing it with that modifier held opens the app there:
    // a tab of the window in front, or a window beside it.
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        let hints: Vec<(ui::search::ActivationModifier, String)> = search
            .palette()
            .visible_entries()
            .iter()
            .find(|entry| entry.label == "Sheets")
            .expect("the app's row")
            .alternates
            .iter()
            .map(|alternate| (alternate.modifier, alternate.label.clone()))
            .collect();
        assert_eq!(
            hints,
            vec![
                (
                    ui::search::ActivationModifier::Shift,
                    "Opens as a tab here".to_owned()
                ),
                (
                    ui::search::ActivationModifier::Ctrl,
                    "Opens a window beside".to_owned()
                ),
            ],
            "the row says what each modifier does before one is held"
        );
        for (held, asked) in [
            (ModifiersState::SHIFT, LaunchPlace::TabHere),
            (ModifiersState::CONTROL, LaunchPlace::Beside),
            (ModifiersState::empty(), LaunchPlace::Focus),
        ] {
            let mut search = DesktopSearch::new();
            search.open(data());
            settled(&mut search);
            search.set_query("sheets");
            settled(&mut search);
            search.palette_mut().set_held_modifiers(held);
            search.palette_mut().activate_selected();
            // Let go before the shell carries the choice out: what was held then still counts.
            search
                .palette_mut()
                .set_held_modifiers(ModifiersState::empty());
            assert!(search.observe(), "choosing a row closes search");
            assert_eq!(
                search.take_choice(),
                Some(Target::Launch {
                    app: "so.tilo.sheets".into(),
                    place: asked,
                }),
                "the modifier held as the row was chosen decides where the app opens"
            );
        }
    }

    phase an_apps_name_opens_the_app_before_any_action_named_the_same;
    {
        // An action can carry an app's name (the empty shell's cards did):
        // a person typing a name wants the app, and Enter on the top row
        // must start it.
        let mut shell = ResourceSpec::new("desktop:shell", "desktop", "Shell");
        shell.actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "notes",
                "Notes",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "shell.action".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: vec![PublishedActions::of("shell", "workspace", &[shell])],
            ..data()
        });
        settled(&mut search);
        search.set_query("notes");
        settled(&mut search);
        let first = search.results()[0].clone();
        assert_eq!(
            (first.provider.as_str(), first.title.as_str()),
            ("applications", "Notes"),
            "the app leads the action of the same name"
        );
        assert_eq!(
            first.target,
            Target::Launch {
                app: "so.tilo.notes".into(),
                place: LaunchPlace::Focus,
            }
        );
    }

    phase a_published_switch_shows_its_state_and_stays_open_on_it;
    {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: published(),
            ..data()
        });
        settled(&mut search);
        search.set_query("full scr");
        settled(&mut search);
        let hit = &search.results()[0];
        assert_eq!(hit.title, "Full screen");
        let (_, painted) = bound_keys(ShortcutAction::ToggleFullscreen);
        let entries = search.palette().visible_entries();
        assert_eq!(entries[0].badge.as_deref(), Some("Off"));
        assert_eq!(
            entries[0].keybinding_label(ui::PlatformConventions::new(ui::Platform::Linux)),
            Some(painted),
            "the keymap's chord, painted as the platform's keycaps"
        );
        assert!(
            entries[0].keybinding.is_none(),
            "a chord is no free-text keybinding"
        );
        drop(entries);
        search.palette_mut().activate_selected();
        assert!(
            !search.observe(),
            "a switch keeps search open on its new state"
        );
        let Some(Target::Invoke(invoke)) = search.take_choice() else {
            panic!("the switch runs by address at its provider");
        };
        assert_eq!(
            (invoke.app.as_str(), invoke.workspace.as_str()),
            ("shell", "workspace")
        );
        let (resource_id, action_id) = ShortcutAction::ToggleFullscreen
            .action()
            .expect("the full screen toggle is a published action");
        assert_eq!(
            invoke.address,
            tilo_actions::ActionAddress::new(resource_id, action_id)
        );
        assert!(search.is_open());
    }

    phase a_published_action_with_nothing_to_act_on_takes_no_row;
    // A published action with nothing to act on now takes no row at all: a card offers what it
    // can do, so Enter always does it — the name of an action that cannot run does not bring back
    // a row that could only say why not.
    {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: published(),
            ..data()
        });
        settled(&mut search);
        for query in ["stop proj", "Stop projecting"] {
            search.set_query(query);
            settled(&mut search);
            assert!(
                search
                    .results()
                    .iter()
                    .all(|hit| hit.title != "Stop projecting"),
                "{query}: it takes no row"
            );
        }
    }

    phase a_dismissed_palette_chooses_nothing_and_remembers_nothing;
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        search.close();
        assert_eq!(search.take_choice(), None);
        search.open(data());
        settled(&mut search);
        assert!(
            search.results().iter().all(|hit| hit.provider != "recent"),
            "nothing was chosen"
        );
    }

    phase an_action_row_hands_its_address_to_the_host_and_closes;
    {
        let mut resource = ResourceSpec::new("page-1", "notes.page", "Groceries");
        resource
            .actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "duplicate",
                "Duplicate note",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.duplicate".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![resource],
            ..data()
        });
        settled(&mut search);
        search.set_query("duplicate");
        settled(&mut search);
        assert_eq!(search.results()[0].title, "Duplicate note");
        search.palette_mut().activate_selected();
        assert!(search.observe());
        let Some(Target::Action(action)) = search.take_choice() else {
            panic!("the host's action API runs it");
        };
        assert_eq!(
            action.address(),
            tilo_actions::ActionAddress::new("page-1", "duplicate")
        );
    }

    phase a_row_that_did_not_happen_keeps_its_reason_and_the_search_stays_open;
    // The host could not carry out the row the person chose: search opens
    // again where it stood, over the same query, and the row keeps the
    // reason under its name instead of the search closing over it.
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("notes");
        settled(&mut search);
        let target = search
            .results()
            .iter()
            .find(|hit| matches!(hit.target, Target::Window { .. }))
            .expect("the open window is a row")
            .target
            .clone();

        search.open(data());
        search.refuse(target.clone(), "its app is gone".to_owned());
        search.set_query("notes");
        settled(&mut search);

        assert!(search.is_open(), "search stays open on the reason");
        assert_eq!(search.query(), "notes", "over the query it stood at");
        assert_eq!(
            search.refusal_for(&target),
            Some("its app is gone"),
            "the row that did not happen is the one that says why"
        );
        let entries = search.palette().visible_entries();
        assert!(
            entries
                .iter()
                .any(|entry| entry.description.as_deref() == Some("its app is gone")),
            "the row says why it did not happen: {:?}",
            entries
                .iter()
                .map(|entry| (&entry.label, &entry.description))
                .collect::<Vec<_>>()
        );
    }

    phase a_row_stands_as_tall_as_the_line_under_its_name;
    // A row stands as tall as the line the palette draws under its name: a
    // place it says there, never one that only repeats the name above.
    {
        let row = |subtitle: Option<&str>, facts: Vec<String>| Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Custom(crate::providers::WINDOWS_KIND.into()),
            provider: "windows".into(),
            id: "w".into(),
            title: "Notes".into(),
            subtitle: subtitle.map(str::to_owned),
            facts,
            score: 1,
            icon: None,
            target: Target::Window { tab: 7, app: None },
        };
        assert!(stands_tall(&row(Some("Tilo/Home"), Vec::new())));
        assert!(!stands_tall(&row(None, Vec::new())));
        assert!(
            !stands_tall(&row(Some("Notes"), Vec::new())),
            "a name said twice says nothing under it"
        );
        assert!(
            stands_tall(&row(None, vec!["Tilo/Home".into()])),
            "a fact is drawn under the name, on the row's second line"
        );
    }

    phase app_rows_wear_their_own_glyph_and_name_without_a_package_id;
    {
        let mut search = DesktopSearch::new();
        search.open(crowded());
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let apps: Vec<_> = entries
            .iter()
            .filter(|entry| entry.id.starts_with("hit:"))
            .filter_map(|entry| match search.hit(&entry.id)?.target {
                Target::Launch { app, .. } => Some((app, *entry)),
                _ => None,
            })
            .collect();
        assert!(!apps.is_empty());
        for (app, entry) in &apps {
            assert_eq!(
                entry.icon,
                Some(ui::icons::app_glyph(
                    app,
                    crate::availability::category_for(app)
                )),
                "{app}"
            );
            assert!(!entry.label.contains("so.tilo"), "{}", entry.label);
            let description = entry.description.as_deref().unwrap_or_default();
            assert!(!description.contains("so.tilo"), "{app}: {description}");
        }
        let notes = apps
            .iter()
            .find(|(app, _)| app == "so.tilo.notes")
            .expect("Notes leads the empty list");
        assert_eq!(notes.1.label, "Notes");
        assert_ne!(
            notes.1.icon,
            Some(ui::icons::Icon::LayoutGrid),
            "not the generic grid"
        );
        // A window wears its app's glyph, not the search glass.
        let window = entries
            .iter()
            .find(|entry| {
                entry.id.starts_with("hit:")
                    && search
                        .hit(&entry.id)
                        .is_some_and(|hit| matches!(hit.target, Target::Window { .. }))
            })
            .expect("windows are listed");
        assert_eq!(window.icon, notes.1.icon);
    }
}

/// A place the person was in lately: what the empty card continues.
fn continue_source(
    recent: &crate::providers::places::RecentPlaces,
) -> crate::providers::places::Continue {
    crate::providers::places::Continue::new(recent, "workspace", vec![9], 100, |_| "Files".into())
}

/// An app's hit says what it is: the row it shaped and the preview of
/// what it holds reach the palette entry unchanged, and choosing it
/// opens the item it names.
struct Mail;

impl ui::search::SearchProvider for Mail {
    fn name(&self) -> &'static str {
        "Mail"
    }
    fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
        query
            .to_lowercase()
            .contains("invoice")
            .then(|| {
                ui::search::SearchResult::for_action(
                    "Mail",
                    "Invoice",
                    tilo_actions::ActionAddress::new("m1", "open"),
                )
                .with_row(
                    ui::search::SearchRow::message("Ada", "Invoice")
                        .snippet("Attached is March")
                        .unread(true),
                )
                .with_preview(
                    ui::search::SearchPreview::titled("Invoice").body("Attached is March"),
                )
            })
            .into_iter()
            .collect()
    }
}

/// The desktop keymap's binding of `action`: the chord a publisher
/// declares, and the way the palette paints that chord. A switch's keys
/// come from the shortcut table, never from a chord written here.
fn bound_keys(action: ShortcutAction) -> (String, String) {
    let chord = ShortcutConfig::default()
        .bindings
        .get(&action)
        .unwrap_or_else(|| panic!("{action:?} is bound"))
        .clone();
    let painted =
        ui::keycaps::active().chord(chord.modifiers.iter().map(String::as_str), &chord.key);
    (chord.display(), painted)
}

/// The shell's published actions: a switch showing its state and the keys
/// the keymap binds it to, and an action with nothing to act on now.
fn published() -> Vec<PublishedActions> {
    let action = |id: &str, label: &str| {
        tilo_actions::ActionSpec::immediate_encoded(
            id,
            label,
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction {
                type_id: "shell.action".into(),
                payload: Vec::new(),
            },
        )
    };
    let (declared, _) = bound_keys(ShortcutAction::ToggleFullscreen);
    let (resource_id, action_id) = ShortcutAction::ToggleFullscreen
        .action()
        .expect("the full screen toggle is a published action");
    let mut resource = ResourceSpec::new(resource_id, resource_id, "Actions");
    resource.actions.push(
        action(action_id, "Full screen").with_state(tilo_actions::ActionState {
            badge: Some("Off".into()),
            toggles: true,
            shortcut: Some(declared),
        }),
    );
    resource.actions.push(
        action("projection.close", "Stop projecting")
            .unavailable("nothing_to_act_on", "Nothing is projected"),
    );
    vec![PublishedActions::of("shell", "workspace", &[resource])]
}

/// A host's own source, named as a person knows it: the notes a notes
/// app holds.
struct Notes;

impl Source for Notes {
    fn id(&self) -> &'static str {
        "notes"
    }

    fn name(&self) -> String {
        "Notes".into()
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        vec![Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Note,
            provider: self.id().into(),
            id: "groceries".into(),
            title: format!("Groceries for {needle}"),
            subtitle: None,
            facts: Vec::new(),
            score: ui::search::system::MAX_PROVIDER_SCORE,
            icon: None,
            target: Target::Window { tab: 7, app: None },
        }]
    }
}

/// A search over the desktop and what one host adds.
fn with_notes(scope: Scope) -> SearchData {
    SearchData {
        sources: vec![Held::new(Notes)],
        scope,
        ..data()
    }
}

tilo_test_env::scenario! {
    fn the_rows_and_the_selection_hold_as_answers_arrive_and_groups_are_cut_short;

    phase typing_refreshes_the_rows_once_per_query;
    // Typing runs the query once: the frame that takes the new text asks
    // the sources, the frames after it fill in what came back, and a
    // settled search asks nothing again.
    {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        assert!(!search.refresh(), "the empty query is already shown");
        search.palette_mut().set_query("not");
        assert!(search.refresh(), "the new text runs the query");
        // A source answers on a worker: its row is in the frame that
        // collects it, which never waits on any other source.
        settled(&mut search);
        assert!(
            search.results().iter().any(|hit| hit.title == "Notes"),
            "the app named by the text is found"
        );
        assert!(
            !search.refresh(),
            "nothing is asked once every source answered"
        );
    }

    phase an_answer_that_arrives_late_keeps_the_show_all_row_the_person_moved_to;
    // The person moved to a group's "Show all" row; an app's answer
    // arriving a frame later adds its group and moves the rows. Enter must
    // still show all of the group they chose, not open the row that took
    // its place.
    {
        // More windows and commands for the query than one card shows.
        let (mut search, arrived) = with_a_late_app(SearchData {
            windows: (0..12)
                .map(|tab| WindowEntry {
                    tab,
                    title: format!("Meeting notes {tab}"),
                    app_title: "Notes".into(),
                    app: Some("so.tilo.notes".into()),
                    place: vec!["Workspace".into(), "Main".into()],
                })
                .collect(),
            commands: (0..12)
                .map(|i| CommandSpec {
                    id: format!("host:{i}"),
                    title: format!("Meeting workspace {i}"),
                    subtitle: None,
                    icon: None,
                    unasked: true,
                })
                .collect(),
            ..data()
        });
        search.set_query("meeting");
        settled(&mut search);
        assert!(search.waiting(), "the app's answer is still coming");
        let mut moves = 0;
        while !search
            .palette()
            .selected_id()
            .is_some_and(|id| id.starts_with(MORE_PREFIX))
        {
            assert!(arrow_down(&mut search), "a group was cut short");
            moves += 1;
        }
        assert!(moves > 0, "the person moved to the row");
        let group = |search: &DesktopSearch| {
            let entries = search.palette().visible_entries();
            entries
                .get(search.palette().cursor())
                .and_then(|entry| entry.group.clone())
        };
        let chosen = group(&search).expect("the row names its group");

        arrived.set(true);
        assert!(search.refresh(), "the answer rebuilds the rows");
        settled(&mut search);
        assert!(!search.waiting(), "the app answered");
        assert!(
            search
                .palette()
                .selected_id()
                .is_some_and(|id| id.starts_with(MORE_PREFIX)),
            "{:?} among {:?}",
            search.palette().selected_id(),
            search
                .palette()
                .visible_entries()
                .iter()
                .map(|entry| (entry.id.clone(), entry.group.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(group(&search).as_deref(), Some(chosen.as_str()));
    }

    phase the_frame_a_query_is_reranked_in_keeps_the_rows_it_shows;
    // An answer to a query re-ranks it, and the frame it re-ranks in has no
    // row yet: the card on screen still shows the rows of the frame before.
    // They stay what the palette presses name, so a key pressed in that
    // frame opens the row the person saw rather than nothing.
    {
        let (mut search, arrived) = with_a_late_app(data());
        search.set_query("notes");
        settled(&mut search);
        let shown = search.results().len();
        let named = search
            .palette()
            .selected_id()
            .expect("a row is chosen")
            .to_owned();
        assert!(shown >= 2, "{:?}", search.results());
        assert!(search.waiting(), "the app's answer is still coming");

        arrived.set(true);
        assert!(search.refresh(), "the answer re-ranks the rows");
        // The frame asks the app's content itself, so its answer may already stand beside
        // them; none of the rows the card showed is gone.
        assert!(
            search.results().len() >= shown,
            "the rows the card shows stay for the frame the query is re-ranked in: {:?}",
            search.results()
        );
        assert_eq!(
            search.palette().selected_id(),
            Some(named.as_str()),
            "the row the person presses Enter on is still named"
        );
        assert!(
            search
                .palette()
                .visible_entries()
                .iter()
                .any(|entry| entry.id == named),
            "the card lists the row it shows"
        );

        settled(&mut search);
        assert!(
            search
                .results()
                .iter()
                .any(|hit| hit.title == "notes from the meeting"),
            "the late answer is listed once the query is ranked again: {:?}",
            search.results()
        );
    }

    phase an_unmoved_selection_follows_the_best_result;
    // Nobody moved the selection: it is the best result, and follows the
    // best result when an answer arrives.
    {
        let (mut search, arrived) = with_a_late_app(data());
        search.set_query("notes");
        settled(&mut search);
        arrived.set(true);
        assert!(search.refresh());
        settled(&mut search);
        let best = search.results().first().expect("rows");
        assert_eq!(
            selected(&search),
            Some((best.provider.clone(), best.id.clone()))
        );
    }

    phase a_crowded_card_lists_the_rest_of_its_results_in_one_row;
    // A card with no room for every group offers the rows it leaves out in
    // one row, and the rows it does keep stand inside it rather than being
    // cut at its edge.
    {
        let mut search = DesktopSearch::new();
        // Room for what stays once a group makes way, not for every group.
        search.set_room(500.0, 320.0);
        search.open(crowded());
        settled(&mut search);
        let shown = search.palette().visible_entries();
        let entries: Vec<CommandEntry> = shown.iter().map(|entry| (*entry).clone()).collect();
        let groups = groups_of(&shown).len();
        assert!(groups >= 1, "{entries:?}");
        assert!(
            search.palette().entries_fit(&entries, groups),
            "every row the card keeps stands inside it: {entries:?}"
        );
        let rest = entries
            .iter()
            .find(|entry| entry.id == ALL_RESULTS_ID)
            .unwrap_or_else(|| panic!("a crowded card offers the rest in one row: {entries:?}"));
        let count: usize = rest
            .label
            .split_whitespace()
            .find_map(|word| word.parse().ok())
            .unwrap_or_default();
        assert!(
            rest.label.starts_with("Show all ") && count > 0,
            "the row says how many results it lists: {}",
            rest.label
        );
    }

    phase the_rest_of_the_results_are_listed_when_their_row_is_chosen;
    // Choosing the row that lists the rest shows every result the search
    // found, groups and all, and leaves the card to scroll them: nothing
    // the query found stays out of reach.
    {
        let mut search = DesktopSearch::new();
        search.set_room(500.0, 160.0);
        search.open(crowded());
        settled(&mut search);
        let before = search.results().len();
        let entries = search.palette().visible_entries();
        let at = entries
            .iter()
            .position(|entry| entry.id == ALL_RESULTS_ID)
            .unwrap_or_else(|| panic!("a crowded card offers the rest: {entries:?}"));
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(!search.observe(), "listing the rest keeps search open");
        assert!(search.is_open(), "the card stays up");
        let after = search.results().len();
        assert!(after > before, "{before} rows became {after}");
        assert!(
            search
                .palette()
                .visible_entries()
                .iter()
                .all(|entry| entry.id != ALL_RESULTS_ID),
            "nothing is left over to offer"
        );
    }

    phase a_show_all_row_is_offered_only_for_a_group_cut_short;
    {
        let mut search = DesktopSearch::new();
        search.open(crowded());
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let more: Vec<_> = entries
            .iter()
            .filter(|entry| entry.id.starts_with(MORE_PREFIX))
            .collect();
        assert!(!more.is_empty(), "the apps were cut short");
        for row in &more {
            let shown = entries
                .iter()
                .filter(|entry| entry.group == row.group && entry.id.starts_with("hit:"))
                .count();
            assert!(shown < row.group_total.unwrap_or_default(), "{}", row.label);
            assert!(
                row.label
                    .contains(&row.group_total.unwrap_or_default().to_string()),
                "it says how many: {}",
                row.label
            );
        }
        // Typing narrows the list; a few rows need no "Show all".
        search.set_query("calendar");
        settled(&mut search);
        assert!(
            search
                .palette()
                .visible_entries()
                .iter()
                .all(|entry| !entry.id.starts_with(MORE_PREFIX))
        );
    }

    phase a_group_asked_for_whole_scrolls_rather_than_making_way;
    // Choosing a group's "Show all" keeps that group on the card even when
    // listing it whole no longer fits: the card scrolls what the person
    // asked for instead of taking it away again.
    {
        let mut search = DesktopSearch::new();
        // Room for the three groups the empty card holds, not for all their
        // rows.
        search.set_room(400.0, 340.0);
        search.open(SearchData {
            applications: vec![AppEntry {
                id: "so.tilo.notes".into(),
                title: "Notes".into(),
                category: None,
            }],
            windows: vec![WindowEntry {
                tab: 1,
                title: "Notes".into(),
                app_title: "Notes".into(),
                app: Some("so.tilo.notes".into()),
                place: Vec::new(),
            }],
            commands: (0..6)
                .map(|i| CommandSpec {
                    id: format!("host:{i}"),
                    title: format!("Command {i}"),
                    subtitle: None,
                    icon: None,
                    unasked: true,
                })
                .collect(),
            ..SearchData::default()
        });
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let (at, group, total) = entries
            .iter()
            .enumerate()
            .find_map(|(at, entry)| {
                if !entry.id.starts_with(MORE_PREFIX) {
                    return None;
                }
                Some((at, entry.group.clone()?, entry.group_total?))
            })
            .unwrap_or_else(|| panic!("a cramped card offers the rest: {entries:?}"));
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(!search.observe(), "showing all keeps search open");
        let shown = search.palette().visible_entries();
        let listed = shown
            .iter()
            .filter(|entry| entry.group.as_deref() == Some(group.as_str()))
            .filter(|entry| entry.id.starts_with("hit:"))
            .count();
        assert_eq!(listed, total, "every row of {group} is on the card");
        assert!(
            shown.iter().all(|entry| !(entry.id.starts_with(MORE_PREFIX)
                && entry.group.as_deref() == Some(group.as_str()))),
            "{group} is not offered itself again"
        );
    }
}

/// An app whose rows for a query come after it is asked, as a framework
/// app's do: it answers on its next frame.
struct LateApp {
    arrived: std::rc::Rc<std::cell::Cell<bool>>,
}

impl ui::search::SearchProvider for LateApp {
    fn name(&self) -> &'static str {
        "Notes"
    }
    fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
        Vec::new()
    }
    fn search_page(
        &self,
        query: &ui::search::SearchQuery,
        _: Option<&ui::search::SearchCursor>,
        _: usize,
        _: usize,
    ) -> ui::search::SearchPage {
        if !self.arrived.get() {
            let mut page = ui::search::SearchPage::empty();
            page.pending = Some(ui::search::PendingToken::new(1));
            return page;
        }
        let row = ui::search::SearchResult::for_action(
            "Notes",
            format!("{} from the meeting", query.text()),
            tilo_actions::ActionAddress::new("meeting", "open"),
        );
        ui::search::SearchPage::of(vec![row], None)
    }
    fn poll_page(&self, _: &ui::search::PendingToken) -> ui::search::PagePoll {
        if self.arrived.get() {
            ui::search::PagePoll::Answered(ui::search::SearchPage::empty())
        } else {
            ui::search::PagePoll::Pending
        }
    }
}

/// Search over `base` with an app whose answer is still coming: what
/// the app publishes, and the window the person searches it from.
pub(super) fn with_a_late_app(
    base: SearchData,
) -> (DesktopSearch, std::rc::Rc<std::cell::Cell<bool>>) {
    let arrived = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut meeting = ResourceSpec::new("meeting", "notes.page", "Meeting");
    meeting
        .actions
        .push(tilo_actions::ActionSpec::immediate_encoded(
            "open",
            "Open",
            tilo_actions::ExecutionLocus::ClientLocal,
            tilo_actions::EncodedAction {
                type_id: "test.open".into(),
                payload: Vec::new(),
            },
        ));
    let mut search = DesktopSearch::new();
    search.open(SearchData {
        resources: vec![meeting],
        providers: vec![Box::new(LateApp {
            arrived: std::rc::Rc::clone(&arrived),
        })],
        ..base
    });
    settled(&mut search);
    (search, arrived)
}

/// A window's own search, listing its one place unasked.
struct Places;

impl ui::search::SearchProvider for Places {
    fn name(&self) -> &'static str {
        "Places"
    }
    fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
        vec![ui::search::SearchResult::new(
            "Places",
            "Home",
            "place:home",
        )]
    }
    fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
        Some(tilo_actions::ActionAddress::new("notes.notebook", "open"))
    }
}

/// Draw the frames a host draws for a search until every source has
/// answered: a source answers on a worker and wakes the shell, so a test
/// drives the frames those wakes ask for.
pub(super) fn settled(search: &mut DesktopSearch) {
    let now = ui::time::now();
    let deadline = now.checked_add(crate::tests::SETTLE).unwrap_or(now);
    loop {
        // The search is settled when no source owes an answer and a
        // frame found nothing to change.
        if !search.refresh() && !search.answering() {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "the search never settled for {:?}",
            search.query()
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

pub(super) fn selected(search: &DesktopSearch) -> Option<(String, String)> {
    let hit = search.hit(search.palette().selected_id()?)?;
    Some((hit.provider, hit.id))
}

/// Every app of the registry the web runs, a few windows and commands:
/// more apps than the palette shows at once, as in a browser.
fn crowded() -> SearchData {
    let applications: Vec<AppEntry> = crate::availability::catalog()
        .iter()
        .filter(|app| crate::availability::runs_on(app.id, &["web"]))
        .map(|app| AppEntry {
            id: app.id.into(),
            title: app.name.into(),
            category: None,
        })
        .collect();
    SearchData {
        applications,
        windows: (0..3)
            .map(|tab| WindowEntry {
                tab,
                title: format!("Window {tab}"),
                app_title: "Notes".into(),
                app: Some("so.tilo.notes".into()),
                place: vec!["Workspace".into(), "Main".into()],
            })
            .collect(),
        commands: (0..10)
            .map(|i| CommandSpec {
                id: format!("host:{i}"),
                title: format!("Command {i}"),
                subtitle: None,
                icon: None,
                unasked: true,
            })
            .collect(),
        ..SearchData::default()
    }
}

/// The groups of the rows `entries`: one header apiece, in the order
/// they stand.
fn groups_of<'a>(entries: &'a [&'a CommandEntry]) -> Vec<&'a str> {
    let mut groups: Vec<&str> = Vec::new();
    for entry in entries {
        if let Some(group) = entry.group.as_deref()
            && groups.last() != Some(&group)
        {
            groups.push(group);
        }
    }
    groups
}

pub(super) fn arrow_down(search: &mut DesktopSearch) -> bool {
    let before = search.palette().cursor();
    search.palette_mut().handle_list_key(
        &ui::winit::keyboard::Key::Named(ui::winit::keyboard::NamedKey::ArrowDown),
        false,
        false,
    );
    search.palette().cursor() != before
}

/// An app whose search action asks the service it connects to (`outside:youtube.search`), and
/// an action of the same app that searches nothing outside.
fn service_search() -> Vec<PublishedActions> {
    let prompted = |id: &str, label: &str| {
        tilo_actions::ActionSpec::prompted_param(
            id,
            label,
            tilo_actions::ActionParam::required("query", tilo_actions::ParamType::Text, "What"),
            "lofi hip hop",
            tilo_actions::ExecutionLocus::ClientLocal,
            |text| tilo_actions::EncodedAction {
                type_id: "test.action".into(),
                payload: text.as_bytes().to_vec(),
            },
        )
    };
    let search = prompted("search", "Search");
    let contract = tilo_actions::ActionContract {
        tags: vec![format!(
            "{}youtube.search",
            tilo_actions::OUTSIDE_CAPABILITY_PREFIX
        )],
        ..search.contract.clone()
    };
    let resource = tilo_actions::ResourceSpec::new("tl:so.tilo.tube:root", "tl.tube.YouTube", "YouTube")
        .with_action(search.with_contract(contract))
        .with_action(prompted("rename", "Rename playlist"));
    vec![PublishedActions::of("so.tilo.tube", "", &[resource])]
}

/// A person types what they want to watch into the system's search: the app that connects to
/// the video site offers its own search of the site's catalogue, run with their words, and an
/// action of the app that searches nothing outside offers no such row.
#[test]
fn an_apps_search_of_its_service_is_offered_with_the_words_typed() {
    let mut search = DesktopSearch::new();
    search.open(SearchData {
        published: service_search(),
        scope: Scope::Workspace,
        ..data()
    });
    settled(&mut search);
    let typed = "lofi hip hop";
    search.set_query(typed);
    settled(&mut search);
    let entries = search.palette().visible_entries();
    let labels: Vec<&String> = entries.iter().map(|entry| &entry.label).collect();
    let at = entries
        .iter()
        .position(|entry| entry.label == "Search YouTube for lofi hip hop")
        .unwrap_or_else(|| panic!("YouTube's search is offered: {labels:?}"));
    assert_eq!(
        labels
            .iter()
            .filter(|label| label.starts_with("Search YouTube"))
            .count(),
        1,
        "only the action that asks the service is a search of it: {labels:?}"
    );
    search.palette_mut().set_cursor(at);
    search.palette_mut().activate_selected();
    assert!(search.observe(), "choosing the search closes the palette");
    let Some(Target::Invoke(invoke)) = search.take_choice() else {
        panic!("the search runs the app's own action");
    };
    assert_eq!(invoke.app, "so.tilo.tube");
    assert_eq!(invoke.address.action.as_str(), "search");
    assert_eq!(
        invoke.arguments.get("query"),
        Some(&serde_json::Value::String(typed.to_owned())),
        "the service is asked the words the person typed"
    );
}
