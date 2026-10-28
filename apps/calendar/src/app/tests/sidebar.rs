use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn calendar_visibility_and_account_collapse_filter_views_and_sidebar() {
    // Case `toggling_calendar_visibility_flips_the_flag`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        let id = app
            .accounts
            .all_calendars()
            .map(|(_, c)| c.id.clone())
            .next()
            .unwrap();
        let before = app
            .accounts
            .all_calendars()
            .find(|(_, c)| c.id == id)
            .unwrap()
            .1
            .visible;
        app.apply_action(
            Action::SetCalendarVisible {
                id: id.clone(),
                visible: false,
            },
            &mut Edits::lend(&mut state),
        );
        let after = app
            .accounts
            .all_calendars()
            .find(|(_, c)| c.id == id)
            .unwrap()
            .1
            .visible;
        assert_ne!(before, after);
    }
    // Case `hiding_a_synced_calendar_drops_its_events_and_rechecking_restores_them`.
    {
        // The standout defect: unchecking a calendar must hide its already-synced
        // events on the spot (and re-checking must bring them back with no
        // re-sync). Assert on the real draw path — the Day timeline's click
        // hit-map, which is exactly what a user clicks.
        fn draws(app: &mut CalendarApp, tr: &mut TextRenderer, theme: &Theme, eid: Id) -> bool {
            let mut scene = Scene::new();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                tr,
                theme,
                &PresenceCtx::empty(),
            );
            app.hits
                .iter()
                .any(|(_, a)| matches!(a, Action::OpenEvent(id) if *id == eid))
        }

        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        app.view = View::Day;
        app.detail_open = false;
        app.selection = None;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();

        // A synced event tagged with an existing (checked) calendar, on the
        // anchored day so the Day timeline lays it out.
        let start = app
            .anchor
            .and_time(NaiveTime::from_hms_opt(10, 0, 0).unwrap());
        let ev = Event {
            id: Uuid::new_v4(),
            title: "Remote sync marker".into(),
            start,
            end: start + Duration::hours(1),
            color: Palette::Blue,
            calendar_id: Some("work/default".into()),
            ..Event::default()
        };
        let eid = ev.id;
        app.model.events.push(ev);
        app.model.events.sort_by_key(|e| e.start);

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();

        assert!(
            draws(&mut app, &mut tr, &theme, eid),
            "shown while its calendar is checked"
        );

        app.apply_action(
            Action::SetCalendarVisible {
                id: "work/default".into(),
                visible: false,
            },
            &mut Edits::lend(&mut state),
        );
        assert!(
            !draws(&mut app, &mut tr, &theme, eid),
            "hiding the source calendar removes the event's block from the view"
        );

        app.apply_action(
            Action::SetCalendarVisible {
                id: "work/default".into(),
                visible: true,
            },
            &mut Edits::lend(&mut state),
        );
        assert!(
            draws(&mut app, &mut tr, &theme, eid),
            "re-checking the calendar restores the event with no re-sync"
        );
    }
    // Case `selecting_a_hidden_collection_never_reveals_its_events`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let mut hidden_event = app.model.events[0].clone();
        hidden_event.id = Uuid::new_v4();
        hidden_event.calendar_id = Some("icloud/holidays".into());
        let day = hidden_event.start.date();
        app.model.events.push(hidden_event);
        app.calendar_filter = Some("icloud/holidays".into());

        assert!(
            app.visible_by_day(&[day])[0].is_empty(),
            "the collection filter narrows results but never bypasses the sidebar visibility setting"
        );
    }
    // Case `opening_a_hidden_collection_explains_why_its_events_are_not_shown`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = demo_document(&app);
        present(&mut app, &mut doc, &automerge::ROOT);
        app.open_item(&crate::actions::calendar_resource_id("icloud/holidays"))
            .expect("hidden collection still opens");
        assert_eq!(app.calendar_filter.as_deref(), Some("icloud/holidays"));
        assert!(
            app.status
                .text()
                .is_some_and(|status| status.contains("is hidden")),
            "the collection view must explain its intentionally empty result"
        );
    }
    // Case `show_all_calendars_operation_clears_a_collection_filter`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = demo_document(&app);
        present(&mut app, &mut doc, &automerge::ROOT);
        app.calendar_filter = Some("work/default".into());
        assert!(
            <crate::resource::CalendarEffect as ui::tilo_resource::Effect>::specs()
                .iter()
                .any(|action| action.id == "show-all-calendars"),
            "the recovery route is advertised through the canonical root"
        );
        app.show_all_calendars();
        assert!(app.calendar_filter.is_none());
        assert_eq!(
            app.status.text(),
            Some(crate::resource::t("calendar-showing-all").as_str())
        );
    }
    // Case `collapsing_an_account_folds_its_calendars_in_the_sidebar`.
    {
        fn account_section(app: &CalendarApp, id: &str) -> ui::widget::NavSection {
            app.show_shell();
            let (sections, _) = app.places.shown().expect("the places are shown");
            sections
                .into_iter()
                .find(|section| section.id == format!("account:{id}"))
                .expect("each account is a section")
        }

        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let acc_id = app.accounts.accounts[0].id.clone();
        let acc_cals = app.accounts.accounts[0].calendars.len();
        assert!(acc_cals > 0, "the first sample account has calendars");

        let section = account_section(&app, &acc_id);
        assert!(
            section.collapsible && !section.collapsed,
            "the account folds"
        );
        assert_eq!(section.items.len(), acc_cals, "its calendars are its rows");

        app.apply_action(
            Action::ToggleAccountCollapsed(acc_id.clone()),
            &mut Edits::lend(&mut state),
        );
        assert!(
            app.accounts.accounts[0].collapsed,
            "the account is now collapsed"
        );
        assert!(
            account_section(&app, &acc_id).collapsed,
            "and its section says so"
        );
        app.apply_action(
            Action::ToggleAccountCollapsed(acc_id.clone()),
            &mut Edits::lend(&mut state),
        );
        assert!(
            !app.accounts.accounts[0].collapsed,
            "toggling again re-expands it"
        );
        assert!(!account_section(&app, &acc_id).collapsed);
    }
}

/// What the person does in the shell's sidebar comes back as the
/// calendar's own actions: choosing a list shows its tasks, a calendar's
/// row shows or hides it, a task's row completes it and its Open action
/// opens it, and folding an account is kept.
#[test]
fn the_sidebar_runs_what_the_person_does_as_the_calendars_actions() {
    let mut app = CalendarApp::new_demo();
    let mut state = demo_document(&app);
    present(&mut app, &mut state, &automerge::ROOT);
    app.accounts.accounts.truncate(1);
    app.detail_open = false;
    app.selection = None;
    app.show_shell();
    let size = (1280, 1400);
    let mut harness = sidebar_harness(&app, size);

    let today = place_row(&harness, "list:today");
    harness.click_at(today.center().x, today.center().y);
    app.take_shell_events(&mut Edits::lend(&mut state));
    assert_eq!(app.list_sel, Some(ListSel::Today), "the list is chosen");

    let (calendar, before) = app.accounts.accounts[0]
        .calendars
        .first()
        .map(|calendar| (calendar.id.clone(), calendar.visible))
        .expect("the sample account has a calendar");
    let row = place_row(&harness, &format!("calendar:{calendar}"));
    harness.click_at(row.center().x, row.center().y);
    app.take_shell_events(&mut Edits::lend(&mut state));
    assert_eq!(
        app.accounts
            .all_calendars()
            .find_map(|(_, item)| (item.id == calendar).then_some(item.visible)),
        Some(!before),
        "the row's check shows or hides the calendar"
    );

    // The list shows what the calendar shows now.
    let mut harness = sidebar_harness(&app, size);
    let (sections, _) = app.places.shown().expect("the places are shown");
    let place = sections
        .iter()
        .find(|section| section.id == "tasks")
        .and_then(|section| section.items.first())
        .map(|item| item.id.clone())
        .expect("today's tasks are in the sidebar");
    let task: Id = place
        .strip_prefix("task:")
        .and_then(|id| id.parse().ok())
        .expect("a task's place");
    let row = place_row(&harness, &place);
    harness.dispatch_to_root(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(row.center().x, row.center().y),
    });
    harness.relayout_at(size).expect("the shell lays out");
    let open = find::<ui::widget::NavList>(harness.root_widget())
        .and_then(|list| list.item_action_rect(&place, "open"))
        .expect("the task's row offers Open");
    harness.click_at(open.center().x, open.center().y);
    app.take_shell_events(&mut Edits::lend(&mut state));
    assert_eq!(app.selection, Some(Sel::Task(task)), "Open opens the task");

    let mut harness = sidebar_harness(&app, size);
    let row = place_row(&harness, &place);
    harness.click_at(row.center().x, row.center().y);
    app.take_shell_events(&mut Edits::lend(&mut state));
    assert!(
        app.model.task(task).is_some_and(|task| task.done),
        "the task's check completes it"
    );
}

// --- end-to-end click routing through the real handle_event -----------
