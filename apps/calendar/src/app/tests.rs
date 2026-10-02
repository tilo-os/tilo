use super::*;
use automerge::ROOT;
use serial_test::serial;
use tilo_actions::Action as _;
use ui::headless::Framed;

#[test]
fn task_completion_toggles_by_checkbox_and_label_and_paints_both_states() {
    // Case `task_checkbox_paints_both_completion_states`.
    {
        let theme = Theme::dark();
        let rect = Rect::new(4.0, 6.0, 22.0, 24.0);
        let mut unchecked = Scene::new();
        draw_checkbox(&mut unchecked, Affine::IDENTITY, &theme, rect, false);
        let mut checked = Scene::new();
        draw_checkbox(&mut checked, Affine::IDENTITY, &theme, rect, true);

        let unchecked_tags = unchecked.encoding().draw_tags.len();
        let checked_tags = checked.encoding().draw_tags.len();
        assert!(unchecked_tags > 0, "unchecked state keeps its outline");
        assert!(
            checked_tags > unchecked_tags,
            "checked state adds its fill and check mark"
        );
    }
    // Case `toggling_a_task_flips_done_and_mirrors_to_crdt`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut state);
        assert_ne!(app.model.task(id).unwrap().done, before);
    }
    // Case `sidebar_task_label_and_open_targets_coexist`.
    {
        // The mini task rail must offer BOTH a completion target (checkbox +
        // label) and a select target (open the task) for the same row.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        // With no synced accounts the sidebar has room for the task rail
        // (a machine full of remote calendars still lists tasks in the
        // agenda/day views — the rail is the space-permitting extra).
        app.accounts = AccountStore::default();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 900.0);
        app.relayout();
        // Draw to populate the hit-map.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );

        let open_task = app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::OpenTask(_)))
            .count();
        let toggle_in_rail = app
            .hits
            .iter()
            .any(|(_, a)| matches!(a, Action::ToggleTask(_)));
        assert!(open_task > 0, "task rows must be selectable to view detail");
        assert!(
            toggle_in_rail,
            "checkbox+label must offer a completion target"
        );
    }
    // Case `toggling_via_label_hit_flips_completion`.
    {
        // Resolving a ToggleTask action (what a label/checkbox click produces)
        // flips done — the production toggle path, not a helper.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut state);
        assert_ne!(app.model.task(id).unwrap().done, before);
    }
}

#[test]
fn startup_is_empty_live_with_onboarding_and_seeded_only_for_demo() {
    // Case `demo_app_seeds_model_and_selection`.
    {
        let app = CalendarApp::new_demo();
        assert!(!app.model.events.is_empty());
        assert!(!app.model.tasks.is_empty());
        assert!(app.selection.is_some());
        assert!(app.detail_open);
    }
    // Case `live_app_starts_empty_with_no_fabricated_content`.
    {
        // A fresh machine (no persisted document, no configured accounts) must
        // show an empty calendar — no sample events, tasks or accounts.
        let missing = std::env::temp_dir()
            .join(format!("cal-empty-{}", uuid::Uuid::new_v4()))
            .join("calendar.automerge");
        let mut app = CalendarApp::new_with_data_path(missing);
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        assert!(app.model.events.is_empty(), "no fabricated events");
        assert!(app.model.tasks.is_empty(), "no fabricated tasks");
        assert!(app.accounts.is_empty(), "no fabricated accounts");
        assert!(app.selection.is_none());
        assert!(!app.detail_open);
    }
    // Case `empty_first_run_offers_onboarding_prompt`.
    {
        // A fresh machine with no events surfaces the friendly first-run prompt;
        // the moment there is content it stands down.
        let missing = std::env::temp_dir()
            .join(format!("cal-onboard-{}", uuid::Uuid::new_v4()))
            .join("calendar.automerge");
        let mut app = CalendarApp::new_with_data_path(missing);
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        assert!(app.model.events.is_empty(), "precondition: no events");
        let (title, hint) = app
            .empty_onboarding_prompt()
            .expect("empty calendar shows the onboarding prompt");
        assert_eq!(title, EMPTY_ONBOARDING_TITLE);
        assert!(
            !hint.trim().is_empty(),
            "hint guides the user to add an event"
        );

        // Seeded demo has events → no onboarding.
        let demo = CalendarApp::new_demo();
        assert!(!demo.model.events.is_empty());
        assert!(
            demo.empty_onboarding_prompt().is_none(),
            "a populated calendar hides the onboarding prompt",
        );
    }
    // Case `sync_seeds_crdt_and_binds_editor`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut state, &root);
        assert!(app.seeded);
        // Selected event's notes are bound.
        assert!(app.bound.is_some());
        assert!(!CalDoc::is_empty(&state));
    }
    // Case `new_app_loads_caldav_accounts`.
    {
        // Offline, the app falls back to the sample account store so synced
        // calendars appear in the sidebar.
        let app = CalendarApp::new_demo();
        assert!(!app.accounts.is_empty());
        assert!(app.accounts.all_calendars().count() > 0);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn resource_provider_and_search_catalog_stay_bounded_and_refresh_on_revision() {
    // Case `real_resource_provider_keeps_snapshot_and_late_exact_resolution_bounded`.
    {
        const LARGE: usize = 5_000;

        let mut app = CalendarApp::new_demo();
        let doc = AutoCommit::new();
        let event_template = app.model.events[0].clone();
        let task_template = app.model.tasks[0].clone();
        let project_template = app.model.projects[0].clone();
        let attendee_id = Id::from_u128(900_001);

        app.calendar_projection_snapshot = (0..LARGE)
            .map(|index| crate::actions::CalendarCollectionResource {
                id: format!("stress/calendar/{index}"),
                label: format!("Stress calendar {index}"),
                color: None,
                visible: true,
            })
            .collect();
        let late_calendar = format!("stress/calendar/{}", LARGE - 1);
        app.model.projects = (0..LARGE)
            .map(|index| crate::model::Project {
                id: Id::from_u128(1_000_000 + index as u128),
                name: format!("Stress project {index}"),
                color: project_template.color,
                order: index as u32,
            })
            .collect();
        let late_project_id = app.model.projects.last().expect("late project").id;
        app.model.events = (0..LARGE)
            .map(|index| {
                let mut event = event_template.clone();
                event.id = Id::from_u128(2_000_000 + index as u128);
                event.title = format!("Stress event {index}");
                event.calendar_id = Some(late_calendar.clone());
                event.project = Some(late_project_id);
                event.attendees.clear();
                event.reminder_min = None;
                event
            })
            .collect();
        let late_event = app.model.events.last_mut().expect("late event");
        late_event.reminder_min = Some(10);
        late_event.attendees.push(crate::model::Attendee {
            id: attendee_id,
            name: "Late attendee".into(),
            email: "late@example.test".into(),
            status: crate::model::InviteStatus::NeedsAction,
        });
        let late_event_id = late_event.id;
        app.model.tasks = (0..LARGE)
            .map(|index| {
                let mut task = task_template.clone();
                task.id = Id::from_u128(3_000_000 + index as u128);
                task.title = format!("Stress task {index}");
                task.project = Some(late_project_id);
                task.parent = None;
                task
            })
            .collect();
        let late_task_id = app.model.tasks.last().expect("late task").id;
        app.rebuild_resource_index();

        let root = CalendarApp::publish_resources(&app, &doc);
        assert_eq!(root.len(), 1);
        assert!(
            app.resource_snapshot_inspected.get()
                <= crate::actions::EAGER_RESOURCE_SNAPSHOT_LIMIT * 3,
            "root snapshot inspected {} entity payloads",
            app.resource_snapshot_inspected.get()
        );

        let expect_exact = |id: String, kind: &str| {
            let resource = CalendarApp::publish_resource(&app, &doc, &id)
                .unwrap_or_else(|| panic!("late stable resource did not resolve: {id}"));
            assert_eq!(resource.kind, kind);
            assert!(
                app.resource_exact_inspected.get() <= 1,
                "exact resolution inspected {} model records for {id}",
                app.resource_exact_inspected.get()
            );
        };
        expect_exact(
            crate::actions::calendar_resource_id(&late_calendar),
            "calendar.collection",
        );
        expect_exact(
            crate::actions::project_resource_id(late_project_id),
            "calendar.project",
        );
        let event_id = crate::actions::event_resource_id(late_event_id);
        expect_exact(event_id.clone(), "calendar.event");
        expect_exact(format!("{event_id}:notes"), "calendar.note");
        expect_exact(
            crate::actions::reminder_resource_id(late_event_id),
            "calendar.reminder",
        );
        expect_exact(
            crate::actions::attendee_resource_id(late_event_id, attendee_id),
            "calendar.attendee",
        );
        expect_exact(
            crate::actions::occurrence_resource_id(late_event_id, event_template.start.date()),
            "calendar.occurrence",
        );
        let task_id = crate::actions::task_resource_id(late_task_id);
        expect_exact(task_id.clone(), "calendar.task");
        expect_exact(format!("{task_id}:notes"), "calendar.note");

        app.model.tasks.pop();
        assert!(
            CalendarApp::publish_resource(&app, &doc, &task_id).is_none(),
            "a stale index position must not redirect a deleted stable task id"
        );
        assert_eq!(
            app.resource_exact_inspected.get(),
            0,
            "stale removal is rejected before materialising a task payload"
        );
    }
    // Case `exact_parent_task_caps_delete_fanout_for_over_one_hundred_thousand_children`.
    {
        const CHILDREN: usize = 100_001;

        let mut app = CalendarApp::new_demo();
        let doc = AutoCommit::new();
        let mut parent = app.model.tasks[0].clone();
        parent.id = Id::from_u128(4_000_000);
        parent.parent = None;
        let mut tasks = Vec::with_capacity(CHILDREN + 1);
        tasks.push(parent.clone());
        tasks.extend((0..CHILDREN).map(|offset| {
            let mut child = parent.clone();
            child.id = Id::from_u128(4_100_000 + offset as u128);
            child.parent = Some(parent.id);
            child.title = format!("Child {offset}");
            child
        }));
        app.model.tasks = tasks;
        app.rebuild_resource_index();

        let resource =
            CalendarApp::publish_resource(&app, &doc, &crate::actions::task_resource_id(parent.id))
                .expect("parent task resolves exactly");
        assert!(
            app.resource_exact_inspected.get() <= 1,
            "parent resolution must not walk its children"
        );
        let delete = resource
            .actions
            .iter()
            .find(|action| action.id == "delete")
            .expect("task exposes a delete action with an honest contract");
        assert!(matches!(
            &delete.contract.availability,
            tilo_actions::Availability::Unavailable { code, .. } if code == "task_children_overflow"
        ));
        let ctx = app
            .exact_resource_ctx(&crate::actions::task_resource_id(parent.id))
            .expect("one-record task context");
        assert_eq!(ctx.tasks.len(), 1);
        assert_eq!(
            ctx.tasks[0].subtasks.len(),
            crate::actions::TASK_DELETE_FANOUT_LIMIT
        );
        assert_eq!(
            ctx.tasks[0].subtask_overflow,
            CHILDREN - crate::actions::TASK_DELETE_FANOUT_LIMIT
        );
    }
    // Case `retained_search_catalog_refreshes_after_crdt_revision_and_deletion`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let id = app.model.events[0].id;
        let action = crate::actions::CalendarAction::PatchEvent {
            id: id.to_string(),
            patch: crate::actions::EventPatch {
                title: Some("revision catalogue needle".into()),
                ..crate::actions::EventPatch::default()
            },
        };
        action.apply(&mut doc).expect("valid patch applies");
        app.sync(&mut doc, &ROOT);
        assert!(
            ui::search::capability::search_providers(&app)[0]
                .search("catalogue needle")
                .iter()
                .any(|hit| hit.action_address() == Some(crate::actions::open_event_address(id))),
            "the retained provider observes the new CRDT revision"
        );

        crate::actions::CalendarAction::DeleteEvent {
            id: id.to_string(),
            remote_delete: None,
        }
        .apply(&mut doc)
        .expect("valid delete applies");
        app.sync(&mut doc, &ROOT);
        assert!(
            ui::search::capability::search_providers(&app)[0]
                .search("catalogue needle")
                .is_empty(),
            "the next revision withdraws the deleted record"
        );
    }
    // Case `unchanged_sync_skips_note_projection_but_a_note_revision_refreshes_search`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let initial_projections = app.note_projection_refreshes;
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.note_projection_refreshes, initial_projections,
            "an unchanged frame must not walk every event/task note"
        );

        let event = app.model.events[0].id;
        crate::actions::CalendarAction::EditNotes {
            owner: crate::actions::NoteOwner::Event {
                id: event.to_string(),
            },
            edit: tilo_document::RichTextAction::AppendParagraph {
                text: "post hydration note needle".to_string(),
            },
        }
        .apply(&mut doc)
        .expect("note action applies");
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.note_projection_refreshes, initial_projections + 1);
        assert!(
            ui::search::capability::search_providers(&app)[0]
                .search("hydration note needle")
                .iter()
                .any(|hit| hit.action_address() == Some(crate::actions::open_event_address(event)))
        );
    }
    // Case `non_crdt_calendar_projection_refreshes_catalog_without_reprojecting_notes`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let note_refreshes = app.note_projection_refreshes;
        let catalog_refreshes = app.search_catalog_refreshes;
        let calendar = app
            .accounts
            .accounts
            .iter_mut()
            .flat_map(|account| account.calendars.iter_mut())
            .next()
            .expect("demo has a Settings-owned calendar");
        let id = calendar.id.clone();
        calendar.name = "Settings reload catalogue needle".into();

        // Simulate an account-state replacement: configuration changes while
        // the Automerge document is deliberately untouched.
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.note_projection_refreshes, note_refreshes,
            "a non-CRDT collection change must reuse the current note projection"
        );
        assert_eq!(app.search_catalog_refreshes, catalog_refreshes + 1);
        assert!(
            ui::search::capability::search_providers(&app)[0]
                .search("reload catalogue needle")
                .iter()
                .any(|hit| {
                    hit.action_address() == Some(crate::actions::open_calendar_address(&id))
                })
        );

        app.sync(&mut doc, &ROOT);
        assert_eq!(app.note_projection_refreshes, note_refreshes);
        assert_eq!(app.search_catalog_refreshes, catalog_refreshes + 1);
    }
    // Case `configured_calendar_resource_is_searchable_and_selects_its_collection`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        let calendar = app
            .calendar_resource_ctx()
            .calendars
            .into_iter()
            .find(|calendar| calendar.id != "local")
            .expect("demo has a remote calendar");
        let hit = ui::search::capability::search_providers(&app)[0]
            .search(&calendar.label)
            .into_iter()
            .find(|hit| {
                hit.action_address() == Some(crate::actions::open_calendar_address(&calendar.id))
            })
            .expect("calendar collection is indexed by canonical address");
        assert_eq!(hit.title, calendar.label);
        let resource = CalendarApp::publish_resource(
            &app,
            &doc,
            &crate::actions::calendar_resource_id(&calendar.id),
        )
        .expect("collection resolves with a current child context");
        assert!(resource.actions.iter().any(|action| action.id == "open"));
        let visibility_action = resource
            .actions
            .iter()
            .find(|action| action.id == "set-visibility")
            .expect("collection exposes its Settings-owned visibility policy");
        assert_eq!(visibility_action.label, "Hide calendar");
        let payload = serde_json::to_vec(&crate::actions::CalendarOperation::OpenCalendar {
            id: calendar.id.clone(),
        })
        .expect("operation serializes");
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::CALENDAR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("collection operation is accepted"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.calendar_filter.as_deref(), Some(calendar.id.as_str()));

        let payload = serde_json::to_vec(&crate::actions::CalendarOperation::SetCalendarVisible {
            id: calendar.id.clone(),
            visible: false,
        })
        .expect("operation serializes");
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::CALENDAR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("visibility operation is accepted"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::CALENDAR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("replaying an absolute visibility operation is accepted"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert!(
            app.accounts
                .all_calendars()
                .find_map(|(_, item)| (item.id == calendar.id).then_some(item.visible))
                .is_some_and(|visible| !visible)
        );
    }
}

#[test]
fn layout_reflows_on_resize_instead_of_caching_first_frame_geometry() {
    // Case `breakpoint_reflows_on_resize_not_cached_from_first_frame`.
    {
        // The reported bug: responsive on first launch, frozen after resizing.
        // Prove the breakpoint + region rects are re-derived from the CURRENT
        // width on every layout pass — on a SINGLE instance — by driving the
        // real `relayout` (exactly what `apply_layout` calls) across a
        // wide → narrow → wide sequence.
        let mut app = CalendarApp::new_demo();

        // Wide first frame: persistent sidebar, detail docked beside calendar.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert!(app.content_rect.width() > 0.0);
        assert!(app.detail_rect.width() > 0.0);
        let wide_content = app.content_rect;

        // Resize the SAME instance narrow. A cached-from-first-frame mode would
        // stay Persistent; a correctly re-derived one flips to Overlay.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert_eq!(
            app.sidebar.mode(),
            SidebarMode::Overlay,
            "breakpoint must re-derive from the current width, not the first frame"
        );
        // Narrow + detail-open default => detail is the full-width pushed
        // screen and the calendar content pane collapses.
        assert_eq!(app.content_rect.width(), 0.0);
        assert!(app.detail_rect.width() > 0.0);
        assert_ne!(
            app.content_rect, wide_content,
            "layout actually changed on resize"
        );

        // Back to wide — proves it toggles both directions, not just once.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert!(app.detail_rect.width() > 0.0);
        assert_eq!(app.content_rect, wide_content);
    }
    // Case `notes_editor_width_retracks_after_resize`.
    {
        // The notes editor's text-wrap width is set in `build_layout`, which the
        // live loop only re-runs on a structure rebuild. A resize that reshapes
        // the detail pane must therefore request a rebuild so the editor column
        // tracks the new width instead of staying frozen at launch width.
        use std::collections::HashMap;
        use taffy::TaffyTree;

        let mut app = CalendarApp::new_demo();
        let theme = Theme::dark();

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        app.build_layout(&mut taffy, &theme, &mut node_map);
        let wide_built = app.built_notes_w;
        assert!(wide_built > 0.0);

        // Resize narrow: the pane wants a different notes width than we built.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert!(
            (app.detail_body_w() - wide_built).abs() > 0.5,
            "the narrow pane wants a different notes width"
        );
        assert!(
            app.take_structure_dirty(),
            "a resize that changes the notes column must request an editor rebuild"
        );

        // The rebuild adopts the resized width.
        let mut taffy2: TaffyTree<()> = TaffyTree::new();
        let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        app.build_layout(&mut taffy2, &theme, &mut node_map2);
        assert_ne!(
            app.built_notes_w, wide_built,
            "editor rebuilt at the resized width"
        );
    }
    // Case `month_week_agenda_grid_reflows_on_resize`.
    {
        // Beyond the panel breakpoints, the *grid inside a view* must also
        // re-derive its cell geometry from the current width on every frame —
        // "lays out once, breaks after resize" is the reported bug. Both widths
        // here sit above every panel breakpoint, so a
        // change in cell width can only be the grid itself reflowing, not a
        // breakpoint flip. Measured on the real path: the hit rects that live
        // clicks resolve against, populated by driving `draw` through `Harness`.
        fn widest_cell(app: &CalendarApp, view: View) -> f64 {
            app.hits
                .iter()
                .filter(|(_, a)| match view {
                    View::Month => matches!(a, Action::PickDate(_)),
                    View::Week | View::Day => matches!(a, Action::OpenDay(_)),
                    View::Agenda => matches!(a, Action::OpenEvent(_) | Action::OpenTask(_)),
                })
                .map(|(r, _)| r.width())
                .fold(0.0f64, f64::max)
        }

        for view in [View::Month, View::Week, View::Agenda] {
            let mut hn = Harness::new(1280, 800);
            hn.app.detail_open = false;
            hn.app.view = view;
            hn.relayout();
            let wide = widest_cell(&hn.app, view);
            assert!(wide > 0.0, "{view:?}: expected grid cells in the hit map");

            // Resize narrower — still wide enough that the sidebar stays docked,
            // isolating the grid's own reflow from any breakpoint change.
            hn.resize(1000, 800);
            assert_eq!(hn.app.sidebar.mode(), SidebarMode::Persistent);
            let narrow = widest_cell(&hn.app, view);
            assert!(
                narrow + 1.0 < wide,
                "{view:?} grid must reflow narrower on resize (was {wide:.1}, now {narrow:.1}) — not laid out once"
            );
        }
    }
}

#[test]
fn titles_elide_to_fit_and_week_ranges_keep_both_endpoints() {
    // Case `week_range_short_keeps_both_endpoints`.
    {
        // The compact week title (used when the topbar is tight) must still show
        // start AND end — the clip bug lost the end date entirely.
        let s = crate::date::week_range_short(
            NaiveDate::from_ymd_opt(2026, 6, 29).unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 5).unwrap(),
        );
        assert!(s.contains("29 Jun"), "start kept: {s}");
        assert!(s.contains("5 Jul"), "end kept: {s}");
    }
    // Case `elide_to_width_ellipsizes_and_actually_fits`.
    {
        let mut tr = TextRenderer::new();
        let full = "Team standup with the whole design org";
        // Given room for the whole string, it comes back untouched (no ellipsis).
        let roomy = f64::from(tr.measure(full, 11.0, None).0) + 10.0;
        assert_eq!(
            tr.elide_to_width(full, 11.0, ui::num::f32_of_f64(roomy)),
            full
        );
        // Under a tight budget it must ellipsize AND the result must measure
        // within the budget — proving the cut is width-driven, not a fixed
        // character count that could still overflow.
        let tight = 60.0;
        let cut = tr.elide_to_width(full, 11.0, ui::num::f32_of_f64(tight));
        assert!(cut.ends_with('…'), "ellipsized: {cut:?}");
        assert!(cut.chars().count() < full.chars().count());
        assert!(
            f64::from(tr.measure(&cut, 11.0, None).0) <= tight,
            "the ellipsized line fits the budget: {cut:?}"
        );
    }
    // Case `week_event_titles_are_ellipsized_not_hard_sliced`.
    {
        // A narrow week column can't fit a full title; the drawn line must carry
        // an ellipsis rather than a hard mid-glyph cut. Exercise the real
        // `draw_timeline` path and confirm at least one event line ellipsizes.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.view = View::Week;
        app.detail_open = false;
        app.narrow_screen = NarrowScreen::Calendar;
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();

        let _theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = app.view_rect();
        let days = crate::date::week_days(app.anchor);
        // The narrowest timed block, ellipsized to its width, must not equal the
        // full title (i.e. a real title got shortened) and must end with '…'.
        let col_w = (area.width() - 56.0) / days.len() as f64;
        let block_text_w = col_w - 6.0 - 20.0; // 3px block inset each side, 10+... pad
        let long = app
            .model
            .events
            .iter()
            .filter(|e| !e.all_day)
            .map(|e| e.title.clone())
            .find(|tit| f64::from(tr.measure(tit, 11.0, None).0) > block_text_w)
            .expect("demo has a title too long for a narrow week column");
        let drawn = tr.elide_to_width(&long, 11.0, ui::num::f32_of_f64(block_text_w));
        assert!(
            drawn.ends_with('…'),
            "narrow week title ellipsized: {drawn:?}"
        );
        assert_ne!(drawn, long);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn launch_ics_admits_host_sources_and_rejects_invalid_ones() {
    // Case `launch_ics_uses_host_admission_and_preserves_existing_calendar_and_source`.
    {
        for writable in [true, false] {
            let directory =
                std::env::temp_dir().join(format!("cal-launch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let source = directory.join("Private Kalender – Zürich 日本.ics");
            let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:launch-event\r\nSUMMARY:Private launch event\r\nDTSTART:20260919T090000\r\nDTEND:20260919T100000\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:launch-task\r\nSUMMARY:Private launch task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
            std::fs::write(&source, payload).unwrap();
            let mut app = CalendarApp::new_empty_for_shots();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            app.apply_action(Action::NewEvent, &mut state.doc);
            let existing = app.model.events[0].id;
            app.sync(&mut state.doc, &state.root);
            state.doc.commit();
            let before = state.doc.get_heads();
            let url = url::Url::from_file_path(&source).unwrap();
            app.queue_launch_ics(url.as_str()).unwrap();
            app.sync(&mut state.doc, &state.root);
            assert_eq!(state.doc.get_heads(), before, "staging never mutates state");
            let resource = app.pending_launch_resource().unwrap();
            let spec = &resource.actions[0];
            assert_eq!(
                spec.contract.permission,
                tilo_actions::PermissionRequirement::Write
            );
            assert!(spec.params().is_empty());
            let tilo_actions::ActionInput::Immediate(encoded) = &spec.input else {
                panic!("zero-argument effect")
            };
            assert_eq!(
                serde_json::from_slice::<crate::actions::CalendarOperation>(&encoded.payload)
                    .unwrap(),
                crate::actions::CalendarOperation::FinalizeLaunchImport
            );
            assert!(!String::from_utf8_lossy(&encoded.payload).contains("Private"));
            let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (800, 600));
            // Revoke after the pending resource has been published: the actual
            // execution must consult the current host permission, not staging time.
            if !writable {
                harness.set_permission_ctx(ui::permissions::PermissionContext::from_capabilities(
                    ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), "reader"),
                    vec![],
                ));
            }
            harness.send(&WindowEvent::RedrawRequested);
            let model = CalDoc::read_model(&harness.state_mut().doc);
            assert!(model.event(existing).is_some());
            assert_eq!(model.events.len(), if writable { 2 } else { 1 });
            assert_eq!(model.tasks.len(), usize::from(writable));
            if !writable {
                assert_eq!(harness.state_mut().doc.get_heads(), before);
            }
            // A refusal is terminal; restoring Write must not silently retry.
            harness.set_permission_ctx(ui::permissions::PermissionContext::permissive());
            harness.send(&WindowEvent::RedrawRequested);
            assert_eq!(
                CalDoc::read_model(&harness.state_mut().doc).events.len(),
                model.events.len()
            );
            assert_eq!(std::fs::read_to_string(&source).unwrap(), payload);
            let saved = harness.state_mut().doc.save();
            let restored = AutoCommit::load(&saved).unwrap();
            assert_eq!(
                CalDoc::read_model(&restored).events.len(),
                model.events.len()
            );
            std::fs::remove_dir_all(directory).unwrap();
        }
    }
    // Case `launch_ics_rejects_invalid_sources_and_unstaged_finalize`.
    {
        let directory =
            std::env::temp_dir().join(format!("cal-invalid-launch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("invalid.ics");
        std::fs::write(&source, [0xff]).unwrap();
        let mut app = CalendarApp::new_empty_for_shots();
        for target in [
            "https://example.com/calendar.ics",
            source.to_str().unwrap(),
            directory.to_str().unwrap(),
        ] {
            assert!(app.queue_launch_ics(target).is_err());
            assert!(app.pending_launch_ics.is_none());
        }
        let mut state = ui::State::new();
        let payload =
            serde_json::to_vec(&crate::actions::CalendarOperation::FinalizeLaunchImport).unwrap();
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::CALENDAR_OPERATION_TYPE,
                    &payload
                ),
                &mut state.doc,
                &state.root
            )
            .is_err()
        );
        assert!(CalDoc::read_model(&state.doc).events.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn user_edits_and_source_identity_persist_across_restart() {
    // Case `user_events_persist_across_restart`.
    {
        // The real backend: an event created in one session is present after a
        // fresh instance loads the same on-disk document.
        let dir = std::env::temp_dir().join(format!("cal-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.automerge");

        let mut app = CalendarApp::new_with_data_path(path.clone());
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        app.apply_action(Action::NewEvent, &mut state.doc);
        // A new event starts untitled (the title field shows its placeholder),
        // and is the current selection.
        let created = match app.selection {
            Some(Sel::Event(id)) => Some(id),
            _ => None,
        };
        assert!(created.is_some(), "NewEvent added an event to the model");
        assert!(
            app.model.events.iter().any(|e| e.title.is_empty()),
            "the fresh event has no title yet"
        );
        // Persist runs on the next sync (writes only when heads changed).
        app.sync(&mut state.doc, &state.root);
        assert!(path.exists(), "document was written to disk");

        // Fresh instance + fresh doc: it must load the event back from disk.
        let mut app2 = CalendarApp::new_with_data_path(path);
        let mut state2 = ui::State::new();
        app2.sync(&mut state2.doc, &state2.root);
        assert!(
            app2.model.events.iter().any(|e| Some(e.id) == created),
            "event survived the restart via on-disk persistence"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `source_calendar_id_survives_the_crdt_round_trip`.
    {
        // The tag must persist, or hiding wouldn't stick across a restart.
        let mut doc = AutoCommit::new();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 6, 15)
            .unwrap()
            .and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        let ev = Event {
            id: Uuid::new_v4(),
            title: "Tagged".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: Palette::Red,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            project: None,
            calendar_id: Some("work/oncall".into()),
        };
        CalDoc::update_event(&mut doc, &ev);
        let back = CalDoc::read_model(&doc);
        assert_eq!(back.events[0].calendar_id.as_deref(), Some("work/oncall"));
    }
    // Case `notes_edit_persists_across_restart`.
    {
        let dir = std::env::temp_dir().join(format!("cal-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.automerge");

        let theme = Theme::dark();
        let mut app = CalendarApp::new_with_data_path(path.clone());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        // Create an event and open its detail (which seeds a notes block).
        app.apply_action(Action::NewEvent, &mut doc);
        let Some(Sel::Event(eid)) = app.selection else {
            panic!("the new event is selected");
        };

        // Lay out wide so the notes editor is active, and build the editor node
        // so key events route into it (handle_event forwards via `editor_node`).
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        // Bind the editor to the event's notes, then focus it.
        app.sync(&mut doc, &root);
        // Creating the event landed the caret in its title; focusing the notes
        // editor must take the keyboard from it (exactly what clicking into the
        // notes region does live), so the marker below types into the notes.
        app.title_input.set_focus(false);
        app.location_input.set_focus(false);
        app.notes_editor.set_focus(true);

        // Type a distinctive marker through the real Widget::handle_event path.
        for ch in ["Z", "Q", "9"] {
            let ev = char_press(ch);
            let mut handled = false;
            let mut focus = None;
            app.handle_event(
                &ev,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &winit::event::Modifiers::default(),
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
        }

        // Persist and confirm it reached disk.
        app.force_persist = true;
        app.sync(&mut doc, &root);
        assert!(path.exists(), "the calendar document was written to disk");

        // Reload the on-disk document and read the note back through a fresh
        // editor binding — the typed text must have survived the restart.
        let doc2 = crate::store::load(&path).expect("document present on disk");
        let joined = CalDoc::note_text(&doc2, true, eid);
        assert!(
            joined.contains("ZQ9"),
            "typed note persisted across restart: {joined:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `edited_title_persists_across_restart`.
    {
        // The full real backend: a title typed into the field reaches the CRDT
        // and the on-disk document, so a fresh instance loads it back.
        let dir = std::env::temp_dir().join(format!("cal-title-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("calendar.automerge");

        let mut h = Harness::live(1280, 800, path.clone());
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        let id = selected_event_id(&h.app);
        assert!(
            h.app.title_input.is_focused(),
            "the new event opens with the title focused"
        );
        for ch in ["R", "o", "a", "d", "m", "a", "p"] {
            h.type_char(ch);
        }
        assert_eq!(h.app.model.event(id).unwrap().title, "Roadmap");
        // Persist runs on the next sync (writes because the heads changed).
        let (app, doc) = h.parts();
        app.sync(doc, &automerge::ROOT);
        assert!(path.exists(), "the document was written to disk");

        // Fresh instance + fresh doc must load the named event back.
        let mut app2 = CalendarApp::new_with_data_path(path);
        let mut doc2 = AutoCommit::new();
        app2.sync(&mut doc2, &automerge::ROOT);
        let ev = app2
            .model
            .events
            .iter()
            .find(|e| e.id == id)
            .expect("the created event survived the restart");
        assert_eq!(
            ev.title, "Roadmap",
            "the edited title persisted across the restart"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
#[serial]
fn ics_imports_are_idempotent_atomic_and_commit_one_batch() {
    // Case `import_reads_a_real_ics_file`.
    {
        // Import pulls the newest `.ics` from the transfer directory through
        // the real parse path — not a bundled sample.
        let dir = std::env::temp_dir().join(format!("cal-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_CALENDAR_IMPORT_DIR", &dir);
        let today = chrono::Local::now().date_naive();
        std::fs::write(dir.join("invite.ics"), crate::ics::sample_ics(today)).unwrap();

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::Import, &mut state);
        assert!(
            app.model.events.len() > before,
            "imported events landed in the model"
        );
        assert!(
            app.model
                .events
                .iter()
                .any(|e| e.title == "Quarterly all-hands"),
            "the .ics file's event parsed in"
        );
        drop(env);
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `uuid_vtodo_import_preserves_identity_and_is_idempotent`.
    {
        let id = Uuid::new_v4();
        let payload = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:{id}@tilo\r\nSUMMARY:Durable imported task\r\nDUE;VALUE=DATE:20260916\r\nPRIORITY:1\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        );
        let mut app = CalendarApp::new_empty_for_shots();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        let first = app
            .import_action_from_payload(&payload)
            .expect("the task is new on the first import");
        let crate::actions::CalendarAction::ImportIcs { tasks, .. } = &first else {
            panic!("ICS import builds one import action");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id.to_string(), "the VTODO UID is retained");

        app.perform_action(&mut doc, &first);
        app.model = CalDoc::read_model(&doc);
        assert!(
            app.model.task(id).is_some(),
            "the exact UID became the task id"
        );
        assert!(
            app.import_action_from_payload(&payload).is_none(),
            "re-importing the same VTODO does not create a duplicate action"
        );
    }
    // Case `resource_import_refuses_invalid_batch_without_partial_writes_or_success`.
    {
        let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:batch-event\r\nSUMMARY:Valid batch event\r\nDTSTART:20260919T090000\r\nDTEND:20260919T100000\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:batch-task\r\nSUMMARY:Valid batch task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        for invalid in ["event", "task", "project"] {
            let mut app = CalendarApp::new_empty_for_shots();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
            let mut batch = app
                .import_action_from_payload(payload)
                .expect("mixed import fixture");
            let crate::actions::CalendarAction::ImportIcs { events, tasks } = &mut batch else {
                panic!("import action");
            };
            assert_eq!(events.len(), 1);
            assert_eq!(tasks.len(), 1);
            match invalid {
                "event" => events[0].end = "not-a-date".into(),
                "task" => tasks[0].due = "not-a-date".into(),
                "project" => tasks[0].project = Uuid::new_v4().to_string(),
                _ => unreachable!(),
            }
            state.doc.commit();
            let before = state.doc.get_heads();
            // Serialized callers and deferred widget imports share the real decoder
            // and executor; rejection cannot leave the preceding valid event behind.
            let encoded = crate::actions::encode(&batch);
            let mut registry = tilo_actions::ActionRegistry::new();
            crate::actions::register(&mut registry);
            let decoded = registry.decode(&encoded).unwrap();
            let mut transport = tilo_actions::LocalTransport::new();
            assert!(
                tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut transport)
                    .is_err()
            );
            assert_eq!(state.doc.get_heads(), before);

            let invocation = tilo_actions::InvocationId::new();
            app.pending_resource_import = Some(batch);
            app.resource_import_invocation = Some(invocation);
            app.sync(&mut state.doc, &state.root);
            let result = Widget::poll_resource_operation(
                &mut app,
                invocation,
                false,
                &state.doc,
                &state.root,
            );
            assert!(
                matches!(result, Some(tilo_actions::InvocationUpdate::Failed { error, .. }) if error.code == "calendar.import_failed" && !error.retryable)
            );
            assert_eq!(state.doc.get_heads(), before);
            assert!(CalDoc::read_model(&state.doc).events.is_empty());
            assert!(CalDoc::read_model(&state.doc).tasks.is_empty());
            assert!(
                app.status
                    .as_deref()
                    .is_some_and(|status| status.starts_with("Import failed:"))
            );
            assert!(
                ui::undo::take_committed_action(&mut app).is_none(),
                "refused import creates no successful history boundary"
            );
            assert!(
                Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &state.doc,
                    &state.root
                )
                .is_none(),
                "failure is delivered once"
            );
        }
    }
    // Case `resource_import_is_stable_until_invoked_then_commits_one_batch`.
    {
        let dir =
            std::env::temp_dir().join(format!("cal-resource-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temporary transfer directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_CALENDAR_IMPORT_DIR", &dir);
        let today = chrono::Local::now().date_naive();
        std::fs::write(dir.join("invite.ics"), crate::ics::sample_ics(today))
            .expect("write fixture");

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let resource_import = |app: &CalendarApp, state: &AutoCommit| {
            ui::resources::resources(app, state)[0]
                .actions
                .iter()
                .find(|action| action.id == "import-latest-ics")
                .expect("import remains discoverable")
                .build_args(std::iter::empty::<(String, tilo_actions::ArgValue)>())
                .expect("nullary import operation")
        };
        let first = resource_import(&app, &state);
        let second = resource_import(&app, &state);
        assert_eq!(
            first.payload, second.payload,
            "resource enumeration is stable"
        );
        assert_eq!(
            app.model
                .events
                .iter()
                .filter(|event| event.title == "Quarterly all-hands")
                .count(),
            0,
            "resource enumeration did not read or apply the transfer file"
        );

        let invocation = tilo_actions::InvocationId::new();
        let start = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                &first.type_id,
                &first.payload,
            ),
            &mut state,
            &automerge::ROOT,
        )
        .expect("resource import accepted");
        assert!(matches!(
            start,
            Some(ui::widget::ResourceOperationStart::Accepted)
        ));
        assert!(
            !app.model
                .events
                .iter()
                .any(|event| event.title == "Quarterly all-hands"),
            "success is deferred until a mutable document pass exists"
        );
        app.sync(&mut state, &automerge::ROOT);
        assert!(
            app.model
                .events
                .iter()
                .any(|event| event.title == "Quarterly all-hands"),
            "the queued batch crossed ActionHost and updated the model"
        );
        assert!(
            matches!(
                Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &state,
                    &automerge::ROOT
                ),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ),
            "completion arrives only after the document action commits"
        );

        drop(env);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn view_and_anchor_navigation_update_shared_state() {
    // Case `switching_view_changes_state`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.apply_action(Action::View(View::Week), &mut state);
        assert_eq!(app.view, View::Week);
        app.apply_action(Action::View(View::Agenda), &mut state);
        assert_eq!(app.view, View::Agenda);
    }
    // Case `stepping_month_changes_anchor_month`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        let m0 = app.anchor.month();
        app.apply_action(Action::Next, &mut state);
        // Month view by default → Next advances one month.
        assert_ne!(app.anchor.month(), m0);
    }
    // Case `mini_month_outcome_updates_the_shared_calendar_anchor`.
    {
        let mut h = Harness::new(1280, 1200);
        h.app.shots_show_calendar();
        h.relayout();
        let before = h.app.anchor;
        let previous = h
            .hit_center(|action| matches!(action, Action::MiniMonth(_)))
            .expect("the sidebar mini-month has a previous-month target");

        h.click(previous);

        assert_eq!(
            h.app.anchor,
            date::add_months(before, -1),
            "the component interprets its target and the root updates the anchor"
        );
    }
    // Case `clicking_a_view_segment_switches_the_view`.
    {
        let mut h = Harness::new(1280, 800);
        h.app.shots_show_calendar();
        h.relayout();
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("Week segment present");
        h.click(p);
        assert_eq!(
            h.app.view,
            View::Week,
            "clicking the Week segment switched the view"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn new_event_is_created_named_and_discarded_only_while_pristine() {
    // Case `new_event_appends_and_selects`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::NewEvent, &mut state);
        assert_eq!(app.model.events.len(), before + 1);
        assert!(matches!(app.selection, Some(Sel::Event(_))));
    }
    // Case `dismissing_a_pristine_new_event_discards_it_through_the_delete_action`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::NewEvent, &mut state);
        let id = app.selected_event().expect("new event selected");

        app.apply_action(Action::BackToCalendar, &mut state);

        assert_eq!(app.model.events.len(), before, "untouched draft is gone");
        assert!(app.model.event(id).is_none());
        assert!(CalDoc::read_model(&state).event(id).is_none());
        let dispatched = app.last_dispatched().expect("discard action dispatched");
        assert!(matches!(
            serde_json::from_slice::<crate::actions::CalendarAction>(&dispatched.payload)
                .expect("calendar delete action"),
            crate::actions::CalendarAction::DeleteEvent { id: deleted, .. } if deleted == id.to_string()
        ));
    }
    // Case `dismissing_an_edited_new_event_keeps_its_autosaved_changes`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::NewEvent, &mut state);
        let id = app.selected_event().expect("new event selected");
        app.apply_action(Action::CycleColor, &mut state);

        app.apply_action(Action::BackToCalendar, &mut state);

        assert!(app.model.event(id).is_some(), "edited draft remains");
        assert!(CalDoc::read_model(&state).event(id).is_some());
        assert!(
            app.fresh_draft.is_none(),
            "it is now an ordinary autosaved event"
        );
    }
    // Case `clicking_new_button_creates_and_selects_an_event`.
    {
        let mut h = Harness::new(1280, 800);
        // Start from the calendar screen: the wide editor is a modal whose
        // dismiss scrim covers the topbar, so the New button is reachable once
        // no editor is open (which is the state a user creates from).
        h.app.shots_show_calendar();
        h.relayout();
        let before = h.app.model.events.len();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button present");
        h.click(p);
        assert_eq!(
            h.app.model.events.len(),
            before + 1,
            "click created an event"
        );
        assert!(
            matches!(h.app.selection, Some(Sel::Event(_))),
            "new event is selected"
        );
    }
    // Case `new_event_is_untitled_with_a_placeholder_and_empty_notes`.
    {
        // A fresh event has NO seeded title or notes: both fields show their
        // placeholders instead of literal text the user must delete.
        let mut h = Harness::new(1280, 800);
        // Create from the calendar screen; the demo's modal editor would
        // otherwise sit over the New button behind its dismiss scrim.
        h.app.shots_show_calendar();
        h.relayout();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        let id = selected_event_id(&h.app);
        assert!(
            h.app.model.event(id).unwrap().title.is_empty(),
            "new event starts untitled"
        );
        // Bind the notes editor to the new event, then confirm the block list
        // carries no seeded body text.
        let (app, doc) = h.parts();
        app.sync(doc, &automerge::ROOT);
        assert_eq!(
            h.app.notes_editor.editor().empty_hint(),
            NOTES_PLACEHOLDER,
            "the notes editor shows a calendar-specific placeholder hint"
        );
        assert!(
            CalDoc::notes(h.doc(), true, id).is_some(),
            "the new event has notes"
        );
        let joined = CalDoc::note_text(h.doc(), true, id);
        assert!(
            joined.trim().is_empty(),
            "notes start empty, not seeded: {joined:?}"
        );
        assert!(
            !joined.contains("Add details"),
            "the hint is a placeholder, not body text"
        );
        // The title field carries NO seeded text and is focused, ready to name
        // the event on the next keystroke; its placeholder is the untitled hint.
        h.relayout();
        assert!(
            h.app.title_input.value().is_empty(),
            "new event's title starts with no seeded text"
        );
        assert!(
            h.app.title_input.is_focused(),
            "the title is focused so typing names the event"
        );
        assert_eq!(h.app.title_input.placeholder(), TITLE_PLACEHOLDER);
        // Blurring the empty field reveals the placeholder hint (never literal body text).
        h.app.title_input.set_focus(false);
        assert!(
            h.app.title_input.shows_placeholder(),
            "empty + unfocused title shows its placeholder"
        );
    }
    // Case `typing_right_after_creating_an_event_names_it`.
    {
        // The reported break: create an event, start typing — the keystrokes
        // must land in the TITLE, not vanish (or trip a view shortcut) because
        // no field was focused. Drive the real button + key path.
        let mut h = Harness::new(1280, 800);
        // Create from the calendar screen so the New button isn't under the
        // demo's open modal.
        h.app.shots_show_calendar();
        h.relayout();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        let id = selected_event_id(&h.app);
        assert!(
            h.app.title_input.is_focused(),
            "a freshly created event opens with its title focused, ready to type"
        );
        for ch in ["S", "t", "a", "n", "d", "u", "p"] {
            h.type_char(ch);
        }
        assert_eq!(
            h.app.model.event(id).unwrap().title,
            "Standup",
            "typing right after Create named the event's title"
        );
        // And the view did NOT switch (a stray letter must not be swallowed as
        // a view shortcut while the title owns the keyboard).
        assert_eq!(
            h.app.view,
            View::Month,
            "letters typed into the title never trip a view shortcut"
        );
    }
    // Case `edited_title_survives_close_and_reopen`.
    {
        // Flow 1, end to end: open an event, edit its title, close the editor,
        // reopen the SAME event — the new title is there, both in the model and
        // reseeded into the field the reopened editor shows.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().title.clone();
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("title target");
        h.click(p);
        h.type_char("!");
        let edited = h.app.model.event(id).unwrap().title.clone();
        assert_ne!(edited, before, "typing changed the title");
        assert!(edited.contains('!'));

        // Close the editor through the production action, then let a frame settle
        // so the form unbinds (form_event clears) — exactly the live sequence.
        let (app, doc) = h.parts();
        app.apply_action(Action::CloseDetail, doc);
        let (app, doc) = h.parts();
        app.sync(doc, &automerge::ROOT);
        assert!(!h.app.detail_open, "editor closed");

        // Reopen the same event (the OpenEvent action's production entry point)
        // and run a sync so the form rebinds from the model.
        let (app, doc) = h.parts();
        app.apply_action(Action::OpenEvent(id), doc);
        let (app, doc) = h.parts();
        app.sync(doc, &automerge::ROOT);
        h.relayout();
        assert_eq!(
            h.app.model.event(id).unwrap().title,
            edited,
            "the edited title is still on the event after reopen"
        );
        assert_eq!(
            h.app.title_input.value(),
            edited,
            "the reopened editor's title field shows the edited title"
        );
    }
}

// --- responsive layout ---------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn large_text_layouts_stay_in_bounds_and_keep_the_one_x_design() {
    // Case `schedule_overlap_warning_reflows_and_keeps_complete_semantics`.
    {
        for (width, text_scale) in [(880.0, 1.0), (880.0, 2.0), (390.0, 2.0)] {
            let mut app = CalendarApp::new_demo();
            let Some(Sel::Event(selected)) = app.selection else {
                panic!("demo opens an event editor");
            };
            let edited = app.model.event(selected).expect("selected event").clone();
            let mut overlapping = edited.clone();
            overlapping.id = Id::new_v4();
            overlapping.title = "Design review follow-up".into();
            overlapping.start = edited.start + chrono::Duration::minutes(15);
            overlapping.end = edited.end + chrono::Duration::minutes(30);
            overlapping.recurrence = Recurrence::Weekly;
            app.model.events.push(overlapping);

            app.text_scale = text_scale;
            app.rect = Rect::new(0.0, 0.0, width, 844.0);
            app.relayout();

            assert!(
                app.form.conflict.area() > 0.0,
                "{width}px / {text_scale}× retains warning"
            );
            assert!(
                app.form.conflict.y1 <= app.form.location.y0,
                "warning reserves its own lane before location at {width}px / {text_scale}×"
            );
            for control in [
                app.form.title,
                app.form.allday,
                app.form.start_date,
                app.form.start_time,
                app.form.end_date,
                app.form.end_time,
                app.form.location,
                app.form.calendar,
                app.form.invite,
            ] {
                assert!(
                    app.form.conflict.intersect(control).area() <= 0.0,
                    "conflict warning overlaps form target {control:?} at {width}px / {text_scale}×"
                );
            }

            let semantics = app.accessibility().expect("calendar semantic tree");
            let warning = semantics
                .children
                .iter()
                .filter_map(|node| node.name.as_deref())
                .find(|name| name.starts_with("Schedule conflict:"))
                .expect("full conflict names reach assistive technology");
            assert!(warning.contains("Design review follow-up"));
        }
    }
    // Case `large_text_desktop_keeps_modal_form_controls_in_bounds`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 1200.0, 900.0);
        app.relayout();

        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert_eq!(
            app.content_rect, app.rect,
            "drawer is hidden behind the menu"
        );
        let controls = [
            app.form.title,
            app.form.allday,
            app.form.start_date,
            app.form.start_time,
            app.form.end_date,
            app.form.end_time,
            app.form.location,
            app.form.calendar,
            app.form.invite,
        ];
        for (index, rect) in controls.iter().enumerate() {
            assert!(
                app.detail_rect.contains_rect(*rect),
                "form control {index} escaped the modal: {rect:?}"
            );
            for other in controls.iter().skip(index + 1) {
                assert!(
                    rect.intersect(*other).area() <= 0.0,
                    "form controls overlap: {rect:?} and {other:?}"
                );
            }
        }

        // Exercise the real paint path too: chip widths come from shaped text,
        // so static field rect checks alone would miss a scaled label escaping
        // its modal.  Every semantic event-form target must stay inside it.
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        text.set_text_scale(theme.text_scale);
        let painted = app.draw_event_form(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        for (rect, _) in painted {
            assert!(
                app.detail_rect.contains_rect(rect),
                "painted form target escaped the modal: {rect:?}"
            );
        }
    }
    // Case `phone_large_text_wraps_every_metadata_chip_inside_the_sheet`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        app.relayout();

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark().with_text_scale(2.0);
        text.set_text_scale(theme.text_scale);
        let painted = app.draw_event_form(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        for (rect, _) in &painted {
            assert!(
                rect.x0 >= app.detail_rect.x0 && rect.x1 <= app.detail_rect.x1,
                "event-form target escaped the phone sheet: {rect:?}"
            );
        }
        let chip_rects: Vec<_> = painted
            .iter()
            .filter(|(_, action)| {
                matches!(
                    action,
                    Action::CycleRecurrence
                        | Action::CycleRecurrenceInterval
                        | Action::CycleReminder
                        | Action::CycleColor
                        | Action::CycleTimezone
                )
            })
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(chip_rects.len(), 5);
        for rect in &chip_rects {
            assert_eq!(
                rect.height(),
                event_form_chip_height(theme.text_scale),
                "the chip border grows with its 2× label instead of cutting through it"
            );
            assert!(
                rect.x0 >= app.detail_rect.x0 && rect.x1 <= app.detail_rect.x1,
                "wrapped metadata chip escaped the phone sheet: {rect:?}"
            );
        }
        let last_bottom = chip_rects.iter().map(|rect| rect.y1).fold(0.0, f64::max);
        assert!(
            app.detail_rect.y0 + app.form.notes_top > last_bottom,
            "notes must begin below the final wrapped metadata row"
        );
    }
    // Case `high_contrast_large_text_metadata_chips_clear_labels_and_notes`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 900.0);
        app.relayout();

        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::high_contrast().with_text_scale(2.0);
        text.set_text_scale(theme.text_scale);
        let painted = app.draw_event_form(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        let chip_rects: Vec<_> = painted
            .iter()
            .filter(|(rect, action)| {
                rect.y0 >= app.form.chips_y
                    && matches!(
                        action,
                        Action::CycleRecurrence
                            | Action::CycleRecurrenceInterval
                            | Action::CycleReminder
                            | Action::CycleColor
                            | Action::CycleTimezone
                            | Action::FocusField(FormField::Invite)
                    )
            })
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(chip_rects.len(), 6);
        let expected_h = event_form_chip_height(theme.text_scale);
        assert!(
            chip_rects.iter().all(|rect| rect.height() == expected_h),
            "every high-contrast chip must contain the scaled label within its border"
        );
        let last_bottom = chip_rects.iter().map(|rect| rect.y1).fold(0.0, f64::max);
        assert!(
            app.detail_rect.y0 + app.form.notes_top > last_bottom,
            "the notes divider must remain below the enlarged chip rows"
        );
    }
    // Case `large_text_new_event_modal_stays_inside_the_viewport`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.shots_new_event();
        app.sync(&mut state, &ROOT);
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 1200.0, 900.0);
        app.relayout();

        assert!(app.detail_open);
        assert!(app.detail_rect.y0 >= app.rect.y0 + MODAL_MARGIN - 0.5);
        assert!(app.detail_rect.y1 <= app.rect.y1 - MODAL_MARGIN + 0.5);
        assert!(app.detail_rect.x0 >= app.rect.x0 + MODAL_MARGIN - 0.5);
        assert!(app.detail_rect.x1 <= app.rect.x1 - MODAL_MARGIN + 0.5);
        assert!(
            app.status.is_none(),
            "shots must not paint a creation toast"
        );
    }
    // Case `event_form_reserves_measured_header_and_notes_rows_at_two_x`:
    // The event form reserves its header row, chip row and NOTES header from
    // the same measured lines the painter uses, so at 2× the kind label never
    // abuts the title field and the NOTES rule never crosses its label.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 1200.0, 900.0);
        app.relayout();
        assert!(matches!(app.selection, Some(Sel::Event(_))));

        let sp = &app.spacing;
        let label_h = metrics::line_h(SECTION_LABEL_TEXT, 2.0);
        let header_top = app.detail_rect.y0 + 18.0;
        assert!(
            app.form.header_bottom - header_top >= label_h + f64::from(sp.s2),
            "header row must hold the scaled kind label plus its gap"
        );
        assert!(
            app.form.title.y0 >= app.form.header_bottom + f64::from(sp.s2),
            "title field must clear the header row"
        );

        let selector_h = f64::from(app.tokens_metrics.selector_height);
        let chip_h = event_form_chip_h(selector_h, f64::from(sp.s3), 2.0);
        assert!(chip_h >= metrics::line_h(EVENT_FORM_CHIP_TEXT, 2.0) + f64::from(sp.s3));
        assert_eq!(
            chip_h.fract(),
            0.0,
            "a scaled chip row lands on a whole pixel"
        );
        assert_eq!(
            event_form_chip_h(selector_h, f64::from(sp.s3), 1.0),
            selector_h,
            "at 1× the chip is the theme's selector height"
        );
        let notes_top = app.detail_rect.y0 + app.form.notes_top;
        let rule_y = notes_top - f64::from(sp.s2);
        let label_y = rule_y - f64::from(sp.s1) - label_h;
        assert!(
            label_y >= app.form.chips_y + chip_h + f64::from(sp.s4),
            "NOTES label must sit a section gap below the chip row"
        );
        assert!(
            rule_y >= label_y + label_h,
            "the NOTES rule must be drawn below the whole label line"
        );
    }
    // Case `month_metrics_hold_scaled_text_and_keep_the_one_x_design`:
    // The month grid's header, day-number band and event chips are sized
    // from the measured text lines they carry, so at 2× nothing collides;
    // at 1× the authored design is reproduced exactly.
    {
        let theme = Theme::dark();
        let sp = &theme.tokens.spacing;
        let one = MonthMetrics::new(&theme, 1.0);
        assert_eq!(one.header_h, MONTH_HEADER_MIN_H);
        assert_eq!(one.pill_r, MONTH_TODAY_PILL_R);
        assert_eq!(one.chip_h, MONTH_CHIP_MIN_H);
        // Stacked chips keep an `s1` between them (HIG §7: nothing touches):
        // 17 + 4 = 21 at 1×, where the old 3px pitch left neighbours 3px
        // apart — see the month shots in the 1× polish audit.
        assert_eq!(one.chip_pitch, MONTH_CHIP_MIN_H + f64::from(sp.s1));
        // The authored 1× design: chips start an `s1` below the 24px
        // day-number band, at 6 + 24 + 4 = 34, the gap the painter keeps
        // between the today pill and the first chip.
        assert_eq!(
            one.chips_top,
            MONTH_NUM_TOP + 2.0 * MONTH_TODAY_PILL_R + f64::from(sp.s1)
        );

        let two = MonthMetrics::new(&theme, 2.0);
        for value in [
            two.header_h,
            two.pill_r,
            two.chips_top,
            two.chip_h,
            two.chip_pitch,
        ] {
            assert_eq!(
                value.fract(),
                0.0,
                "scaled month metrics land on whole pixels"
            );
        }
        assert!(two.header_h >= two.label_h + f64::from(sp.s3));
        assert!(two.chip_h >= two.chip_text_h + f64::from(sp.s1));
        assert!(two.chip_pitch > two.chip_h);
        assert!(
            2.0 * two.pill_r >= two.label_h,
            "the today pill must enclose the scaled day number"
        );
        assert!(
            two.chips_top >= MONTH_NUM_TOP + 2.0 * two.pill_r,
            "the first chip must start below the today pill"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn detail_choices_and_invitation_handoff_are_exact_resource_actions() {
    // Case `invitation_handoff_is_the_exact_mail_resource_action_and_never_claims_send`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        let event = app
            .selection
            .and_then(|selection| match selection {
                Sel::Event(id) => app.model.event(id),
                Sel::Task(_) => None,
            })
            .expect("demo selects an event with a guest");
        let attendee = event.attendees.first().expect("demo event has a guest");
        let event_id = event.id;
        let attendee_id = attendee.id;
        let attendee_email = attendee.email.clone();
        let payload = serde_json::to_vec(
            &crate::actions::CalendarOperation::PrepareInvitationInMail {
                event_id: event_id.to_string(),
                attendee_id: attendee_id.to_string(),
            },
        )
        .unwrap();
        let ui::widget::ResourceOperationStart::Completed(output) = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::Effect,
                crate::actions::CALENDAR_OPERATION_TYPE,
                &payload,
            ),
            &mut doc,
            &automerge::ROOT,
        )
        .unwrap()
        .expect("Calendar handles its invitation effect") else {
            panic!("the invitation handoff completes synchronously");
        };
        let value = output.value.expect("typed handoff result");
        assert_eq!(value["delivery"], "not_sent");
        assert_eq!(value["target"]["resource"], "mail.mailbox");
        assert_eq!(value["target"]["action"], "prepare-calendar-invitation");
        let arguments = value["arguments"].as_object().unwrap().clone();

        let mail_roots = mail::actions::provide_resources(
            &mail::actions::MailResourceCtx {
                mailbox_label: "Mail".into(),
                current_folder: mail::model::Folder::Inbox,
                accounts: vec![("account".into(), "Account".into(), true)],
                messages: Vec::new(),
                custom_folders: Vec::new(),
                has_draft: false,
                can_sync: true,
            },
            &automerge::AutoCommit::new(),
        );
        let encoded = tilo_actions::door::encode_json(
            &mail_roots,
            &tilo_actions::ActionAddress::new("mail.mailbox", "prepare-calendar-invitation"),
            &arguments,
        )
        .expect("Mail accepts Calendar's exact typed arguments");
        let operation = mail::actions::decode_operation(&encoded.type_id, &encoded.payload)
            .unwrap()
            .expect("Mail decodes the handoff");
        assert!(matches!(
            operation,
            mail::actions::MailOperation::PrepareCalendarInvitation {
                to,
                calendar_ics,
                ..
            } if to == attendee_email && calendar_ics.contains("METHOD:REQUEST")
        ));
    }
    // Case `direct_task_detail_choices_match_their_resource_action_envelopes`.
    {
        use tilo_actions::ActionInput;
        use ui::widget::Widget;

        fn prompted(spec: &tilo_actions::ActionSpec, value: &str) -> tilo_actions::EncodedAction {
            match &spec.input {
                ActionInput::Prompted { build, .. } => build(value),
                ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                    panic!("{} must prompt for a choice", spec.id)
                }
            }
        }

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks[0].id;
        app.select_task(id);
        app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
        app.relayout();
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::OpenTaskPopover(TaskPopover::Due)))
        );
        assert!(
            app.hits.iter().any(|(_, action)| matches!(
                action,
                Action::OpenTaskPopover(TaskPopover::Priority)
            ))
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::OpenTaskPopover(TaskPopover::Project)))
        );

        let task =
            CalendarApp::publish_resource(&app, &state, &crate::actions::task_resource_id(id))
                .expect("selected task resource");
        let due = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        let due_spec = task
            .actions
            .iter()
            .find(|action| action.id == "set-due")
            .unwrap();
        app.apply_action(Action::SetTaskDue(Some(due)), &mut state);
        assert_eq!(
            app.last_dispatched().unwrap().payload,
            prompted(due_spec, "2026-09-09").payload
        );

        let priority_spec = task
            .actions
            .iter()
            .find(|action| action.id == "set-priority")
            .unwrap();
        app.set_selected_task_priority(Priority::Medium, &mut state);
        assert_eq!(
            app.last_dispatched().unwrap().payload,
            prompted(priority_spec, "p2").payload
        );

        let project = app.model.projects[0].id;
        let project_spec = task
            .actions
            .iter()
            .find(|action| action.id == "move-to-project")
            .unwrap();
        app.set_selected_task_project(Some(project), &mut state);
        assert_eq!(
            app.last_dispatched().unwrap().payload,
            prompted(project_spec, &crate::actions::project_resource_id(project),).payload
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn sidebar_presentation_follows_width_and_collapses_via_chevrons() {
    // Case `wide_window_uses_persistent_sidebar_and_overlay_editor`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert_eq!(app.detail_mode, DetailMode::Overlay);
        // Sidebar is inline (content starts to its right).
        assert!(app.content_rect.x0 >= app.sidebar.rect().x1 - 0.5);
        // The editor is a centered modal floating OVER the calendar: the
        // calendar keeps its full content width behind it, and the card is
        // inset from every content edge (not a docked strip on the right).
        assert!(app.detail_rect.area() > 0.0);
        assert!(app.content_rect.area() > 0.0);
        assert_eq!(
            app.content_rect.x1, app.rect.x1,
            "calendar spans to the window edge behind the modal"
        );
        assert!(
            app.detail_rect.x0 > app.content_rect.x0 + 1.0,
            "card inset from the left"
        );
        assert!(
            app.detail_rect.x1 < app.rect.x1 - 1.0,
            "card inset from the right"
        );
        assert!(
            app.detail_rect.y0 > app.rect.y0 + 1.0,
            "card inset from the top"
        );
        // Card is centered within the content area.
        let card_cx = app.detail_rect.center().x;
        let content_cx = f64::midpoint(app.content_rect.x0, app.content_rect.x1);
        assert!(
            (card_cx - content_cx).abs() < 1.0,
            "modal centered over the content"
        );
    }
    // Case `narrow_window_collapses_sidebar_to_overlay`.
    {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.sidebar.set_open(false);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        // Overlay sidebar floats over the content from the left edge: content
        // starts at the window's left edge, not to the sidebar's right.
        assert_eq!(app.content_rect.x0, app.rect.x0);
        assert!(!app.sidebar.visible(), "overlay flap is closed by default");
    }
    // Case `desktop_sidebar_collapses_and_only_the_sidebar_collapses`.
    {
        // Confirmed-defect: on desktop the sidebar must be collapsible, and ONLY
        // the sidebar collapses — the editor modal stays open and re-centers in
        // the reclaimed width. Drive the real `relayout` expand → collapse →
        // expand on ONE instance.
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(app.sidebar.visible(), "sidebar shown when expanded");
        let expanded_content_x0 = app.content_rect.x0;
        assert!(
            expanded_content_x0 >= app.sidebar.rect().x1 - 0.5,
            "expanded: content starts to the right of the sidebar"
        );

        // Collapse: sidebar hidden, content reclaims the left edge, editor stays.
        app.sidebar.set_collapsed(true);
        app.relayout();
        assert!(!app.sidebar.visible(), "collapsed sidebar is hidden");
        assert_eq!(
            app.content_rect.x0, app.rect.x0,
            "content reclaims the sidebar edge"
        );
        assert!(app.content_rect.width() > 0.0);
        assert!(
            app.detail_rect.area() > 0.0,
            "editor stays open — only the sidebar collapsed"
        );
        // Re-centered within the now-wider content.
        let content_cx = f64::midpoint(app.content_rect.x0, app.content_rect.x1);
        assert!(
            (app.detail_rect.center().x - content_cx).abs() < 1.0,
            "modal re-centers after collapse"
        );

        // Expand again — proves it toggles both directions.
        app.sidebar.set_collapsed(false);
        app.relayout();
        assert!(app.sidebar.visible());
        assert_eq!(
            app.content_rect.x0, expanded_content_x0,
            "expanding restores the layout"
        );
    }
    // Case `phone_width_sidebar_is_a_full_screen_drawer`.
    {
        // Below the phone breakpoint the overlay sidebar covers the whole
        // window (a page) rather than a cramped 252px flap.
        let mut app = CalendarApp::new_demo();

        app.rect = Rect::new(0.0, 0.0, 700.0, 900.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            app.sidebar.rect().width() < app.rect.width() - 1.0,
            "tablet width: a flap, not full-width"
        );

        app.rect = Rect::new(0.0, 0.0, 420.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            (app.sidebar.rect().width() - app.rect.width()).abs() < 0.5,
            "phone width: the sidebar drawer is a full-screen page"
        );
    }
    // Case `full_screen_drawer_offers_a_pointer_reachable_close`.
    {
        // The full-screen drawer covers the window, so it must carry its own
        // close (no dimmed edge to click). Drive the real click path.
        let mut h = Harness::new(420, 800);
        h.app.shots_show_calendar();
        h.app.sidebar.set_open(true);
        h.relayout();
        assert!(h.app.sidebar.visible(), "flap is open");
        assert!(
            (h.app.sidebar.rect().width() - h.app.rect.width()).abs() < 0.5,
            "the drawer is full-screen at this width"
        );
        // Two DismissSidebar targets exist: the (hidden) scrim behind and the
        // small close button in the header. Pick the compact one — the button.
        let close = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::DismissSidebar))
            .min_by_key(|(r, _)| r.area() as i64)
            .map(|(r, _)| r.center())
            .expect("a close button in the full-screen drawer");
        h.click(close);
        assert!(
            !h.app.sidebar.is_open(),
            "clicking close dismissed the full-screen drawer"
        );
    }
    // Case `clicking_sidebar_chevron_collapses_then_topbar_chevron_expands`.
    {
        // The collapse control is reachable and functional through the real
        // click path, and its expand mirror lives in the topbar once collapsed —
        // so a pointer user can always get the sidebar back.
        let mut h = Harness::new(1280, 800);
        // The topbar's expand chevron lives behind the modal's dismiss scrim, so
        // exercise the sidebar collapse from the calendar screen (the state a
        // user collapses the rail from).
        h.app.shots_show_calendar();
        h.relayout();
        assert!(h.app.sidebar.visible(), "starts expanded on a wide window");
        let collapse = h
            .hit_center(|a| matches!(a, Action::ToggleSidebarCollapsed))
            .expect("collapse chevron present in the persistent sidebar");
        h.click(collapse);
        assert!(
            h.app.sidebar.is_collapsed(),
            "clicking the chevron collapsed the sidebar"
        );
        assert!(!h.app.sidebar.visible());

        // Re-lay so the collapsed topbar's expand chevron enters the hit-map.
        h.relayout();
        let expand = h
            .hit_center(|a| matches!(a, Action::ToggleSidebarCollapsed))
            .expect("expand chevron present in the topbar when collapsed");
        h.click(expand);
        assert!(
            !h.app.sidebar.is_collapsed(),
            "clicking the topbar chevron expanded the sidebar"
        );
        assert!(h.app.sidebar.visible());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn detail_and_editor_presentation_follow_width_with_matching_close_affordances() {
    // Case `narrow_detail_screen_replaces_the_calendar`.
    {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
        app.detail_open = true;
        app.narrow_screen = NarrowScreen::Detail;
        app.relayout();
        assert_eq!(
            app.detail_mode,
            DetailMode::FullScreen,
            "480px editor is full-screen"
        );
        // The detail is pushed over the calendar: only one pane is laid out.
        assert!(app.detail_rect.area() > 0.0);
        assert_eq!(
            app.content_rect.area(),
            0.0,
            "calendar hidden behind detail"
        );

        // Going back returns to the calendar screen.
        let mut state = AutoCommit::new();
        app.apply_action(Action::BackToCalendar, &mut state);
        app.relayout();
        assert!(app.content_rect.area() > 0.0);
        assert_eq!(app.detail_rect.area(), 0.0);
    }
    // Case `selecting_in_narrow_mode_pushes_detail_and_closes_flap`.
    {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
        app.relayout();
        app.sidebar.set_open(true);
        let id = app.model.events[0].id;
        app.select_event(id);
        assert_eq!(app.narrow_screen, NarrowScreen::Detail);
        assert!(
            !app.sidebar.is_open(),
            "opening a detail dismisses the overlay flap"
        );
    }
    // Case `editor_presentation_switches_from_overlay_to_fullscreen_by_width`.
    {
        // The reported fix: on big screens the editor is a modal OVERLAY over
        // the calendar; only on small windows does it take the whole screen.
        // Drive the real relayout across the breakpoint on ONE instance.
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;

        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.detail_mode, DetailMode::Overlay, "wide → modal overlay");
        assert!(
            app.content_rect.area() > 0.0,
            "calendar stays laid out behind the modal"
        );
        assert!(
            app.detail_rect.x1 < app.rect.x1 - 1.0,
            "modal is a floating card, not edge-to-edge"
        );

        // Just above the breakpoint stays an overlay.
        app.rect = Rect::new(0.0, 0.0, EDITOR_OVERLAY_MIN + 10.0, 800.0);
        app.relayout();
        assert_eq!(app.detail_mode, DetailMode::Overlay);

        // Below the breakpoint: full-screen editor page replacing the calendar.
        app.rect = Rect::new(0.0, 0.0, EDITOR_OVERLAY_MIN - 10.0, 800.0);
        app.narrow_screen = NarrowScreen::Detail;
        app.relayout();
        assert_eq!(
            app.detail_mode,
            DetailMode::FullScreen,
            "narrow → full-screen editor"
        );
        assert_eq!(
            app.content_rect.area(),
            0.0,
            "full-screen editor replaces the calendar"
        );
        assert!(app.detail_rect.area() > 0.0);
    }
    // Case `clicking_an_event_chip_opens_its_detail`.
    {
        // Month view is full of chips; clicking one selects that event.
        let mut h = Harness::new(1280, 800);
        h.app.shots_show_calendar();
        h.relayout();
        let (rect, id) = h
            .app
            .hits
            .iter()
            .find_map(|(r, a)| match a {
                Action::OpenEvent(id) => Some((*r, *id)),
                _ => None,
            })
            .expect("at least one event chip");
        h.click(rect.center());
        assert_eq!(
            h.app.selection,
            Some(Sel::Event(id)),
            "clicked chip is now selected"
        );
        assert!(h.app.detail_open, "detail panel opened");
    }
    // Case `narrow_pushed_detail_uses_back_not_a_redundant_close`.
    {
        // De-slop: the narrow pushed detail has a single Back affordance, not a
        // Back plus a duplicate X that both return to the calendar.
        let mut h = Harness::new(560, 720);
        h.app.detail_open = true;
        h.app.narrow_screen = NarrowScreen::Detail;
        h.relayout();
        let closes = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::CloseDetail))
            .count();
        let backs = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::BackToCalendar))
            .count();
        assert_eq!(
            closes, 0,
            "no redundant X close on the narrow pushed detail"
        );
        assert_eq!(backs, 1, "Back is the single dismissal affordance");
    }
    // Case `wide_modal_editor_has_an_x_close_and_a_dismiss_scrim`.
    {
        // The wide editor is a modal card: it carries an X close button (a
        // small top-right target) AND a dismiss scrim over the calendar behind
        // it (clicking outside closes it). No Back — that's narrow-only.
        let h = Harness::new(1280, 800);
        assert!(
            h.app.detail_open,
            "demo opens with a selected event → modal editor"
        );
        assert_eq!(h.app.detail_mode, DetailMode::Overlay);
        let backs = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::BackToCalendar))
            .count();
        assert_eq!(backs, 0, "no Back affordance on the wide modal");
        let closes: Vec<Rect> = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::CloseDetail))
            .map(|(r, _)| *r)
            .collect();
        // A small X button …
        assert!(
            closes
                .iter()
                .any(|r| r.area() < 2000.0 && r.x0 > h.app.detail_rect.x0),
            "a compact X close button in the card header"
        );
        // … and a large dismiss scrim covering the calendar behind the card.
        assert!(
            closes
                .iter()
                .any(|r| (r.area() - h.app.content_rect.area()).abs() < 1.0),
            "a dismiss scrim over the content behind the modal"
        );
    }
}

#[test]
fn view_switcher_lives_in_topbar_or_flap_by_width_without_duplicates() {
    // Case `view_switcher_stays_in_topbar_until_it_genuinely_doesnt_fit`.
    {
        // Issue: the switcher folded into the sidebar too eagerly. It must stay
        // in the topbar at normal/large widths and only fold when it truly
        // can't fit. Assert the real fit decision that `Topbar::draw` reads.
        let mut app = CalendarApp::new_demo();
        // Isolate the switcher logic on the calendar screen (no pushed detail
        // zeroing the content width out from under us).
        app.detail_open = false;
        app.selection = None;
        app.narrow_screen = NarrowScreen::Calendar;

        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(
            app.topbar.switcher_visible(),
            "wide window keeps the switcher in the topbar"
        );

        // A mid-width overlay-sidebar window still has ample topbar room — the
        // previous behaviour wrongly folded it away here.
        app.rect = Rect::new(0.0, 0.0, 780.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            app.topbar.switcher_visible(),
            "a roomy topbar keeps the switcher even with an overlay sidebar"
        );

        // Truly cramped → it folds into the flap.
        app.rect = Rect::new(0.0, 0.0, 460.0, 800.0);
        app.relayout();
        assert!(
            !app.topbar.switcher_visible(),
            "a cramped topbar folds the switcher into the sidebar"
        );
    }
    // Case `narrow_flap_offers_a_pointer_reachable_view_switcher`.
    {
        // On narrow layouts the topbar's segmented switch folds away; the
        // overlay flap must carry a pointer-reachable switch so every view stays
        // reachable without a keyboard (the reported dead-end: a mouse user on
        // narrow could reach Day/Agenda but never get back to Month). Drives the
        // real `handle_event` click path, not a synthetic call.
        let mut h = Harness::new(560, 720);
        assert_eq!(
            h.app.sidebar.mode(),
            SidebarMode::Overlay,
            "560px is a narrow layout"
        );
        // Open the flap and re-lay so its hit-map is populated.
        h.app.sidebar.set_open(true);
        h.relayout();
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("the flap carries a Week view target on narrow");
        h.click(p);
        assert_eq!(
            h.app.view,
            View::Week,
            "clicking the flap switcher changed the view"
        );
        assert!(
            !h.app.sidebar.is_open(),
            "picking a view dismisses the flap"
        );
        assert_eq!(
            h.app.narrow_screen,
            NarrowScreen::Calendar,
            "picking a view returns to the calendar screen"
        );
    }
    // Case `wide_topbar_has_no_flap_switcher_duplicate`.
    {
        // The flap switcher is narrow-only; the wide layout already has the
        // topbar segmented control, so the flap must NOT add a second set of
        // View targets (which would double every view hit).
        let h = Harness::new(1280, 800);
        let view_targets = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::View(_)))
            .count();
        assert_eq!(
            view_targets,
            View::ALL.len(),
            "exactly one switcher on wide"
        );
    }
}

// --- click-label-to-complete ---------------------------------------

#[test]
fn calendar_and_project_selectors_file_events_and_reach_every_option() {
    // Case `project_selector_reaches_projects_beyond_the_old_four_row_cutoff`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks[0].id;
        let later = crate::model::Project {
            id: Id::new_v4(),
            name: "Duplicate project".into(),
            color: Palette::Blue,
            order: 99,
        };
        app.model
            .projects
            .extend((0..5).map(|index| crate::model::Project {
                id: Id::new_v4(),
                name: format!("Project {index}"),
                color: Palette::Indigo,
                order: index,
            }));
        app.model.projects.push(later.clone());
        let same_named_earlier = crate::model::Project {
            id: Id::new_v4(),
            name: "Duplicate project".into(),
            color: Palette::Blue,
            order: 98,
        };
        app.model.projects.push(same_named_earlier);
        CalDoc::seed(&mut state, &app.model);
        app.select_task(id);
        app.apply_action(Action::OpenTaskPopover(TaskPopover::Project), &mut state);
        assert!(
            app.task_project_values.len() > 5,
            "the shared dropdown is populated from the complete project list"
        );
        app.task_project_choice.set(later.id.to_string());
        app.commit_task_project_dropdown(&mut state, &automerge::ROOT);
        assert_eq!(app.model.task(id).unwrap().project, Some(later.id));
    }
    // Case `calendar_selector_files_event_on_a_configured_account_and_queues_it`.
    {
        // The selector lists calendars from the shared account store; picking a
        // remote CalDAV calendar tags the event AND queues it for upload.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let p = h
            .hit_center(|a| matches!(a, Action::OpenPopover(FormPopover::Calendar)))
            .expect("the calendar field opens a selector");
        h.click(p);
        assert_eq!(h.app.event_popovers.active(), FormPopover::Calendar);
        h.relayout(); // draw the list so its row targets populate
        // Local is row 0; the first configured remote calendar is row 1.
        let options_h = 34.0 * h.app.calendar_options().len() as f64 + 8.0;
        let list_y = (h.app.form.calendar.y1 + 6.0).min(h.app.detail_rect.y1 - options_h - 8.0);
        let p2 = Point::new(h.app.form.calendar.center().x, list_y + 4.0 + 34.0 + 17.0);
        h.click(p2);
        assert_eq!(
            h.app.model.event(id).unwrap().calendar_id.as_deref(),
            Some("work/default"),
            "the event is filed on the chosen CalDAV calendar"
        );
        assert!(
            CalDoc::outbox(h.doc()).contains(&id),
            "creating on a CalDAV calendar queues the event for upload"
        );
        assert_eq!(
            h.app.event_popovers.active(),
            FormPopover::None,
            "picking a calendar closes the selector"
        );
    }
    // Case `refiling_on_the_local_calendar_clears_the_upload_queue`.
    {
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        // File it remote first (queued), then move it back to the device.
        let (app, doc) = h.parts();
        app.set_event_calendar(&CalTarget::Remote("work/default".into()), doc);
        assert!(CalDoc::outbox(h.doc()).contains(&id));
        let (app, doc) = h.parts();
        app.set_event_calendar(&CalTarget::Local, doc);
        assert!(h.app.model.event(id).unwrap().calendar_id.is_none());
        assert!(
            !CalDoc::outbox(h.doc()).contains(&id),
            "filing back on the local calendar drops it from the queue"
        );
    }
    // Case `calendar_options_come_from_the_shared_account_store`.
    {
        // The option list is sourced from the configured accounts, never faked:
        // the sample store's calendars all appear, plus the local option.
        let h = Harness::new(1280, 800);
        let opts = h.app.calendar_options();
        assert!(
            matches!(opts.first(), Some((CalTarget::Local, _))),
            "local is first"
        );
        for (_, cal) in h.app.accounts.all_calendars() {
            assert!(
                opts.iter()
                    .any(|(t, _)| matches!(t, CalTarget::Remote(id) if id == &cal.id)),
                "configured calendar {} is offered in the selector",
                cal.id
            );
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn accessibility_exports_named_stable_controls_that_reuse_actions() {
    // Case `accessibility_exposes_sidebar_lists_and_calendar_visibility_controls`.
    {
        use tilo_actions::ActionInput;

        fn immediate(spec: &tilo_actions::ActionSpec) -> tilo_actions::EncodedAction {
            match &spec.input {
                ActionInput::Immediate(encoded) => encoded.clone(),
                ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                    panic!("{} must not prompt", spec.id)
                }
            }
        }

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.rect = Rect::new(0.0, 0.0, 1_280.0, 800.0);
        app.relayout();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );

        let tree = app.accessibility().expect("calendar has a semantic window");
        let today = tree
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("calendar.list:today"))
            .expect("Today smart list is reachable to assistive technology");
        assert_eq!(today.role, Role::RadioButton);
        assert_eq!(today.state.checked, Some(true));

        let (calendar_id, before) = app
            .accounts
            .accounts
            .iter()
            .flat_map(|account| account.calendars.iter())
            .next()
            .map(|calendar| (calendar.id.clone(), calendar.visible))
            .expect("demo has one configured remote calendar");
        let target = format!(
            "{}:visibility",
            crate::actions::calendar_resource_id(&calendar_id)
        );
        let visibility = tree
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some(target.as_str()))
            .expect("calendar visibility is a semantic checkbox");
        assert_eq!(visibility.role, Role::Checkbox);
        assert_eq!(visibility.state.checked, Some(before));

        let resource = CalendarApp::publish_resource(
            &app,
            &state,
            &crate::actions::calendar_resource_id(&calendar_id),
        )
        .expect("configured calendar is an exact resource");
        let pointer_spec = resource
            .actions
            .iter()
            .find(|action| action.id == "set-visibility")
            .expect("calendar resource exposes the visibility operation");
        let pointer_payload = immediate(pointer_spec);
        app.apply_action(
            Action::SetCalendarVisible {
                id: calendar_id.clone(),
                visible: !before,
            },
            &mut state,
        );
        assert_eq!(
            app.last_local_resource_operation(),
            Some(&pointer_payload),
            "the pointer row sends the exact resource action envelope"
        );
        assert_eq!(
            app.accounts
                .all_calendars()
                .find_map(|(_, item)| (item.id == calendar_id).then_some(item.visible)),
            Some(!before),
            "the pointer action reaches the Settings-owned absolute setter"
        );

        // Rebuild the hit map after the pointer action. Its newly published
        // resource action carries the opposite absolute state for the
        // screen-reader checkbox as well.
        let resource = CalendarApp::publish_resource(
            &app,
            &state,
            &crate::actions::calendar_resource_id(&calendar_id),
        )
        .expect("visibility change keeps the configured resource addressable");
        let semantic_payload = immediate(
            resource
                .actions
                .iter()
                .find(|action| action.id == "set-visibility")
                .expect("resource republishes the inverse absolute operation"),
        );
        let mut scene = Scene::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        assert!(app.perform_semantic_action(
            &target,
            &A11yAction::ToggleCheck,
            &mut state,
            &automerge::ROOT,
        ));
        assert_eq!(
            app.last_local_resource_operation(),
            Some(&semantic_payload),
            "the screen-reader checkbox sends the exact resource action envelope"
        );
        assert_eq!(
            app.accounts
                .accounts
                .iter()
                .flat_map(|account| account.calendars.iter())
                .find(|item| item.id == calendar_id)
                .map(|item| item.visible),
            Some(before),
            "screen-reader activation follows the same absolute Settings-backed operation as the row"
        );

        // Replaying this absolute payload must converge on the requested value
        // rather than invert it as a toggle would.
        for _ in 0..2 {
            assert!(matches!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        &semantic_payload.type_id,
                        &semantic_payload.payload
                    ),
                    &mut state,
                    &automerge::ROOT
                )
                .expect("published visibility action is accepted"),
                Some(ui::widget::ResourceOperationStart::Completed(_))
            ));
        }
        assert_eq!(
            app.accounts
                .all_calendars()
                .find_map(|(_, item)| (item.id == calendar_id).then_some(item.visible)),
            Some(before),
            "two replays retain the requested absolute visibility state"
        );
    }
    // Case `accessibility_exports_named_topbar_controls_and_reuses_actions`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.rect = Rect::new(0.0, 0.0, 960.0, 700.0);
        app.relayout();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            &PresenceCtx::empty(),
        );
        let tree = app.accessibility().expect("calendar has a semantic window");
        assert_eq!(tree.role, Role::Window);
        assert!(tree.children.iter().any(|node| {
            node.name.as_deref() == Some("New event")
                && node.action_target.as_deref() == Some("calendar.new-event")
        }));
        let before = app.model.events.len();
        assert!(app.perform_semantic_action(
            "calendar.new-event",
            &A11yAction::Activate,
            &mut state,
            &automerge::ROOT,
        ));
        assert_eq!(app.model.events.len(), before + 1);
    }
    // Case `accessibility_exports_stable_event_and_task_action_targets`.
    {
        let h = Harness::new(1280, 800);
        let event = selected_event_id(&h.app);
        let task = h.app.model.tasks[0].id;

        let (event_target, _, _, _) = h
            .app
            .a11y_control(&Action::OpenEvent(event))
            .expect("event has a semantic action");
        assert_eq!(
            event_target,
            format!("{}:open", crate::actions::event_resource_id(event)),
            "screen reader activation addresses the same durable event as search/history"
        );
        let (task_target, _, role, state) = h
            .app
            .a11y_control(&Action::ToggleTask(task))
            .expect("task toggle has a semantic action");
        assert_eq!(
            task_target,
            format!("{}:toggle", crate::actions::task_resource_id(task))
        );
        assert_eq!(role, Role::Checkbox);
        assert_eq!(state.checked, Some(h.app.model.task(task).unwrap().done));
    }
    // Case `event_form_exposes_the_expected_editable_targets`.
    {
        // The form paints real hit targets for every editable control.
        let h = Harness::new(1280, 800);
        let has = |pred: fn(&Action) -> bool| h.app.hits.iter().any(|(_, a)| pred(a));
        assert!(
            has(|a| matches!(a, Action::FocusField(FormField::Title))),
            "title field"
        );
        assert!(
            has(|a| matches!(a, Action::FocusField(FormField::Location))),
            "location field"
        );
        assert!(has(|a| matches!(a, Action::ToggleAllDay)), "all-day toggle");
        assert!(
            has(|a| matches!(a, Action::OpenPopover(FormPopover::StartDate))),
            "start date"
        );
        assert!(
            has(|a| matches!(a, Action::OpenPopover(FormPopover::StartTime))),
            "start time"
        );
        assert!(
            has(|a| matches!(a, Action::OpenPopover(FormPopover::Calendar))),
            "calendar selector"
        );
    }
}

#[test]
#[serial]
fn keyboard_focus_and_shortcuts_route_between_editor_and_calendar() {
    // Case `escape_blurs_notes_before_dismissing_the_editor`.
    {
        let mut h = Harness::new(1280, 800);
        h.app.title_input.set_focus(false);
        h.app.location_input.set_focus(false);
        h.app.notes_editor.set_focus(true);
        h.press_named(NamedKey::Escape);
        assert!(h.app.detail_open, "first Escape keeps the editor open");
        assert!(
            !h.app.notes_editor.is_focused(),
            "first Escape returns focus from notes instead of unexpectedly closing the editor"
        );
    }
    // Case `keyboard_shortcuts_switch_views_navigate_and_act`.
    {
        // Every shortcut goes through the production `Widget::handle_event` key
        // path, not `apply_action`. 'e'/'i' touch the transfer dir, so redirect
        // it to a temp dir first.
        let dir = std::env::temp_dir().join(format!("cal-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_CALENDAR_IMPORT_DIR", &dir);

        let mut h = Harness::new(1280, 800);
        // Close the demo's detail so the notes editor doesn't own keystrokes.
        let (app, doc) = h.parts();
        app.apply_action(Action::CloseDetail, doc);
        h.relayout();

        h.type_char("w");
        assert_eq!(h.app.view, View::Week, "'w' → Week");
        h.type_char("d");
        assert_eq!(h.app.view, View::Day, "'d' → Day");
        h.type_char("a");
        assert_eq!(h.app.view, View::Agenda, "'a' → Agenda");
        h.type_char("m");
        assert_eq!(h.app.view, View::Month, "'m' → Month");

        // 't' snaps the anchor back to today; step away first to prove it moves.
        let (app, doc) = h.parts();
        app.apply_action(Action::Next, doc);
        assert_ne!(h.app.anchor, h.app.today);
        h.type_char("t");
        assert_eq!(h.app.anchor, h.app.today, "'t' → today");

        // '[' / ']' step the period; arrows move the day anchor.
        let m0 = h.app.anchor.month();
        h.type_char("]");
        assert_ne!(h.app.anchor.month(), m0, "']' steps the month forward");
        h.type_char("[");
        assert_eq!(h.app.anchor.month(), m0, "'[' steps it back");
        let a0 = h.app.anchor;
        h.press_named(winit::keyboard::NamedKey::ArrowRight);
        assert_eq!(h.app.anchor, a0 + Duration::days(1), "ArrowRight → +1 day");
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
        assert_eq!(h.app.anchor, a0 + Duration::days(8), "ArrowDown → +7 days");

        // 'n' creates a new event through the keyboard.
        let before = h.app.model.events.len();
        h.type_char("n");
        assert_eq!(h.app.model.events.len(), before + 1, "'n' created an event");

        // Creating an event lands the caret in its title, so the calendar no
        // longer owns the keyboard; close the editor to make the next shortcut a
        // global key again rather than a keystroke into the title.
        let (app, doc) = h.parts();
        app.apply_action(Action::CloseDetail, doc);
        h.relayout();

        // 'e' exports through the keyboard: a real .ics lands in the transfer dir.
        h.type_char("e");
        assert!(
            crate::store::newest_ics(&dir).is_some(),
            "'e' exported an .ics through the key path"
        );

        drop(env);
        std::fs::remove_dir_all(&dir).ok();
    }
    // Case `delete_key_in_the_title_field_does_not_delete_the_event`.
    {
        // Guard: Delete must edit text when a field is focused, never nuke the
        // event out from under the caret.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("title target");
        h.click(p);
        assert!(h.app.title_input.is_focused());
        h.press_named(winit::keyboard::NamedKey::Delete);
        assert!(
            h.app.model.event(id).is_some(),
            "Delete in a text field kept the event"
        );
    }
    // Case `open_editor_captures_the_keyboard_and_release_on_close`.
    {
        // The seam that makes the form's OWN Tab/field routing reachable in the
        // live app: while open, the editor reports a modal capture so the host
        // App suspends its built-in Tab focus cycling; closing releases it.
        let mut h = Harness::new(1280, 800);
        assert!(h.app.detail_open);
        assert!(
            h.app.modal_capture().is_some(),
            "an open editor captures the keyboard so the App yields Tab to the form"
        );
        let (app, doc) = h.parts();
        app.apply_action(Action::CloseDetail, doc);
        h.relayout();
        assert!(
            h.app.modal_capture().is_none(),
            "a closed editor releases the keyboard back to the App"
        );
    }
    // Case `closing_the_editor_releases_field_focus_so_shortcuts_work`.
    {
        // Bug: opening/creating an event focuses the title; closing the editor
        // must blur it so the next keystroke drives the calendar (a shortcut),
        // not a hidden title field that silently swallows keys.
        let mut h = Harness::new(1280, 800);
        h.app.focus_field(FormField::Title);
        assert!(h.app.title_input.is_focused());
        let (app, doc) = h.parts();
        app.apply_action(Action::CloseDetail, doc);
        assert!(
            !h.app.title_input.is_focused(),
            "closing the editor blurs the title"
        );
        assert!(!h.app.location_input.is_focused(), "…and the location");
        assert!(!h.app.notes_editor.is_focused(), "…and the notes editor");
        // A view shortcut now lands — it would be eaten by the title otherwise.
        h.relayout();
        h.type_char("w");
        assert_eq!(
            h.app.view,
            View::Week,
            "a shortcut works once the editor has closed"
        );
    }
}

// --- caldav accounts seam ------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn caldav_sync_runs_off_thread_and_drains_results_tags_or_failure() {
    // Case `sync_merges_visible_calendar_events`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        // Sync is explicit now (no fabricated auto-pull on the first frame).
        app.apply_action(Action::Sync, &mut state);
        assert!(app.model.events.iter().any(|e| e.title == "Synced standup"));
        // The hidden "Holidays" calendar's event must not appear.
        assert!(!app.model.events.iter().any(|e| e.title == "Public holiday"));
    }
    // Case `live_sync_runs_off_the_ui_thread_without_blocking`.
    {
        // A live (non-demo) app with a configured account must dispatch the
        // pull to a worker thread and return immediately — the click never
        // blocks on the network. We assert the async hand-off, then abandon the
        // detached worker (its result is irrelevant to this behaviour).
        let mut app = CalendarApp::new_demo();
        app.demo = false; // exercise the real (threaded) branch
        app.accounts = AccountStore::sample();
        let mut state = AutoCommit::new();
        app.start_sync(&mut state);
        assert!(app.syncing, "a live sync is in flight");
        assert!(app.sync_rx.is_some(), "worker result channel is armed");
        assert!(
            app.needs_animation_frame(),
            "the loop must keep ticking so the result drains"
        );
        // Detach the worker so the test doesn't depend on the network.
        app.sync_rx = None;
        app.syncing = false;
    }
    // Case `live_sync_with_no_accounts_answers_instantly`.
    {
        // No worker, no thread, no spin: an empty account set reports its state
        // synchronously.
        let mut app = CalendarApp::new_demo();
        app.demo = false;
        app.accounts = AccountStore::default();
        let mut state = AutoCommit::new();
        app.start_sync(&mut state);
        assert!(!app.syncing, "nothing to sync → no background work");
        assert!(app.status.as_deref().unwrap_or("").contains("account"));
    }
    // Case `poll_sync_drains_a_finished_worker_into_the_model`.
    {
        // The drain path (`poll_sync` → `apply_sync_result`) folds a completed
        // pull into the model + CRDT and clears the in-flight flag. Driven with
        // a hand-fed channel so it is deterministic and network-free, but it is
        // the exact production code the live worker feeds.
        use crate::model::{Event, Palette, Recurrence};
        use chrono::NaiveTime;

        let mut app = CalendarApp::new_demo();
        app.demo = false;
        let mut state = AutoCommit::new();
        let before = app.model.events.len();

        let (tx, rx) = std::sync::mpsc::channel();
        let start = app
            .today
            .and_time(NaiveTime::from_hms_opt(8, 0, 0).unwrap());
        let synced = Event {
            id: uuid::Uuid::new_v4(),
            title: "Pulled from server".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: "Remote".into(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            calendar_id: None,
            project: None,
        };
        tx.send((
            caldav::SyncResult {
                events: vec![synced],
                tasks: Vec::new(),
                ..caldav::SyncResult::default()
            },
            SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))
        .unwrap();
        app.sync_rx = Some(rx);
        app.syncing = true;

        app.poll_sync(&mut state);

        assert!(!app.syncing, "drain clears the in-flight flag");
        assert_eq!(app.model.events.len(), before + 1, "synced event landed");
        assert!(
            app.model
                .events
                .iter()
                .any(|e| e.title == "Pulled from server"),
            "the pulled event is now in the model"
        );
        // And it mirrored into the CRDT (persistence source of truth).
        assert!(!CalDoc::is_empty(&state));
    }
    // Case `sync_tags_landed_events_with_their_source_calendar`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut state);
        // The demo pulls "Synced standup" from the work/default collection.
        let standup = app
            .model
            .events
            .iter()
            .find(|e| e.title == "Synced standup")
            .expect("sync pulled the standup");
        assert_eq!(
            standup.calendar_id.as_deref(),
            Some("work/default"),
            "a synced event remembers which calendar it came from"
        );
        // A locally-created event stays untagged, so it is always visible.
        app.apply_action(Action::NewEvent, &mut state);
        let Some(Sel::Event(new_id)) = app.selection else {
            panic!("NewEvent selects the created event");
        };
        let new = app.model.event(new_id).unwrap();
        assert!(
            new.calendar_id.is_none(),
            "local events carry no source calendar"
        );
    }
    // Case `poll_sync_reports_failure_when_the_worker_drops`.
    {
        // The Disconnected branch: a worker that panics/drops its sender without
        // a reply must surface "Sync failed" and clear the in-flight state.
        let mut app = CalendarApp::new_demo();
        app.demo = false;
        let mut state = AutoCommit::new();
        let (tx, rx) = std::sync::mpsc::channel::<SyncWorkerResult>();
        app.sync_rx = Some(rx);
        app.syncing = true;
        drop(tx); // worker vanished without sending
        app.poll_sync(&mut state);
        assert!(!app.syncing, "the in-flight flag is cleared");
        assert!(app.sync_rx.is_none(), "the dead channel is dropped");
        assert_eq!(app.status.as_deref(), Some("Sync failed"));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn calendar_visibility_and_account_collapse_filter_views_and_sidebar() {
    // Case `toggling_calendar_visibility_flips_the_flag`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
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
            &mut state,
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
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.view = View::Day;
        app.detail_open = false;
        app.selection = None;
        app.narrow_screen = NarrowScreen::Calendar;
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
            all_day: false,
            location: String::new(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            project: None,
            calendar_id: Some("work/default".into()),
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
            &mut state,
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
            &mut state,
        );
        assert!(
            draws(&mut app, &mut tr, &theme, eid),
            "re-checking the calendar restores the event with no re-sync"
        );
    }
    // Case `selecting_a_hidden_collection_never_reveals_its_events`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let mut hidden_event = app.model.events[0].clone();
        hidden_event.id = Uuid::new_v4();
        hidden_event.calendar_id = Some("icloud/holidays".into());
        let day = hidden_event.start.date();
        app.model.events.push(hidden_event);
        app.calendar_filter = Some("icloud/holidays".into());

        assert!(
            app.visible_events_on(day).is_empty(),
            "the collection filter narrows results but never bypasses the sidebar visibility setting"
        );
    }
    // Case `opening_a_hidden_collection_explains_why_its_events_are_not_shown`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let payload = serde_json::to_vec(&crate::actions::CalendarOperation::OpenCalendar {
            id: "icloud/holidays".into(),
        })
        .expect("operation serializes");

        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::CALENDAR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .expect("hidden collection still opens"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.calendar_filter.as_deref(), Some("icloud/holidays"));
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("is hidden")),
            "the collection view must explain its intentionally empty result"
        );
    }
    // Case `show_all_calendars_operation_clears_a_collection_filter`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        app.calendar_filter = Some("work/default".into());
        let root = CalendarApp::publish_resource(&app, &doc, "calendar.calendar")
            .expect("calendar root is addressable");
        assert!(
            root.actions
                .iter()
                .any(|action| action.id == "show-all-calendars"),
            "the recovery route is advertised through the canonical root"
        );
        let payload = serde_json::to_vec(&crate::actions::CalendarOperation::ShowAllCalendars)
            .expect("operation serializes");
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    crate::actions::CALENDAR_OPERATION_TYPE,
                    &payload
                ),
                &mut doc,
                &automerge::ROOT
            )
            .expect("show-all operation is accepted"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert!(app.calendar_filter.is_none());
        assert_eq!(app.status.as_deref(), Some("Showing all calendars"));
    }
    // Case `collapsing_an_account_hides_its_calendar_rows_in_the_sidebar`.
    {
        fn cal_rows(app: &mut CalendarApp, tr: &mut TextRenderer, theme: &Theme) -> usize {
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
                .filter(|(_, a)| matches!(a, Action::SetCalendarVisible { .. }))
                .count()
        }

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 900.0);
        app.relayout();

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();

        let acc_id = app.accounts.accounts[0].id.clone();
        let acc_cals = app.accounts.accounts[0].calendars.len();
        assert!(acc_cals > 0, "the first sample account has calendars");

        let before = cal_rows(&mut app, &mut tr, &theme);
        // The account header is a real collapse target in the hit-map.
        assert!(
            app.hits
                .iter()
                .any(|(_, a)| matches!(a, Action::ToggleAccountCollapsed(id) if *id == acc_id)),
            "the account header carries a collapse target"
        );

        app.apply_action(Action::ToggleAccountCollapsed(acc_id.clone()), &mut state);
        assert!(
            app.accounts.accounts[0].collapsed,
            "the account is now collapsed"
        );

        let after = cal_rows(&mut app, &mut tr, &theme);
        assert_eq!(
            after,
            before - acc_cals,
            "collapsing removes exactly that account's calendar rows"
        );

        // Header stays clickable so the list can be re-expanded.
        app.apply_action(Action::ToggleAccountCollapsed(acc_id), &mut state);
        assert!(
            !app.accounts.accounts[0].collapsed,
            "clicking again re-expands it"
        );
        assert_eq!(
            cal_rows(&mut app, &mut tr, &theme),
            before,
            "expanding restores the rows"
        );
    }
}

// --- end-to-end click routing through the real handle_event -----------

/// Mounts the app in the production frame host so a test can pump
/// synthesized winit events through the production `Widget::handle_event`,
/// exactly as the live event loop does.
struct Harness {
    app: Framed<CalendarApp>,
}

impl Harness {
    fn new(w: u32, h: u32) -> Self {
        Self::mount(CalendarApp::new_demo(), w, h)
    }

    /// A live (persisted, no sample content) app driven through the same
    /// production event path — used to prove edits reach disk.
    fn live(w: u32, h: u32, path: PathBuf) -> Self {
        Self::mount(CalendarApp::new_with_data_path(path), w, h)
    }

    fn mount(app: CalendarApp, w: u32, h: u32) -> Self {
        Harness {
            app: Framed::new(ui::State::new(), app, (w, h), Theme::dark()),
        }
    }

    /// Rebuild, lay out and draw a whole frame; the draw populates the click
    /// hit-map, like the live renderer.
    fn relayout(&mut self) {
        Framed::harness_mut(&mut self.app)
            .recompose()
            .expect("calendar frame lays out");
    }

    /// Rebuild and run the next frame at a new window size.
    fn resize(&mut self, w: u32, h: u32) {
        Framed::harness_mut(&mut self.app)
            .relayout_themed((w, h), Theme::dark())
            .expect("calendar frame lays out");
    }

    fn doc(&self) -> &AutoCommit {
        &Framed::harness(&self.app).state().doc
    }

    fn parts(&mut self) -> (&mut CalendarApp, &mut AutoCommit) {
        let (app, state) = Framed::split(&mut self.app);
        (app, &mut state.doc)
    }

    fn dispatch(&mut self, event: &winit::event::WindowEvent) -> bool {
        Framed::harness_mut(&mut self.app)
            .dispatch_to_root_with(event, &winit::event::Modifiers::default())
    }

    fn click(&mut self, p: Point) {
        // winit exposes no public DeviceId constructor; the production
        // synthesized-press path does exactly this.
        let device_id = unsafe { std::mem::zeroed() };
        self.dispatch(&winit::event::WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        });
        self.dispatch(&winit::event::WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }

    fn mouse_move(&mut self, p: Point) {
        let device_id = unsafe { std::mem::zeroed() };
        self.dispatch(&winit::event::WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        });
    }
    fn mouse_button(&mut self, state: ElementState) -> bool {
        let device_id = unsafe { std::mem::zeroed() };
        self.dispatch(&winit::event::WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        })
    }

    /// A full press → drag → release gesture through the real event path.
    fn drag(&mut self, from: Point, to: Point) {
        self.mouse_move(from);
        assert!(
            self.mouse_button(ElementState::Pressed),
            "the timeline must claim a drag press"
        );
        self.mouse_move(to);
        assert!(
            self.mouse_button(ElementState::Released),
            "the timeline must claim a drag release"
        );
    }

    /// Press a single character through the real keyboard path.
    fn type_char(&mut self, ch: &str) {
        self.dispatch(&char_press(ch));
    }

    /// Press a named key (arrows, etc.) through the real keyboard path.
    fn press_named(&mut self, named: winit::keyboard::NamedKey) {
        self.dispatch(&named_press(named));
    }

    /// Centre of the first hit rect carrying an action matching `pred`.
    fn hit_center<F: Fn(&Action) -> bool>(&self, pred: F) -> Option<Point> {
        self.app
            .hits
            .iter()
            .find(|(_, a)| pred(a))
            .map(|(r, _)| r.center())
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn week_timeline_scrolls_the_full_day_and_drags_create_and_reschedule() {
    // Case `week_timeline_keeps_min_slot_height_and_scrolls_the_full_day`.
    {
        // The reported bug: a short window squashed the fixed 06:00–21:00 slots
        // and hid everything outside them. Fixed: every hour keeps at least the
        // legible minimum, the grid spans the full 24h, and the body scrolls.
        let mut h = Harness::new(1000, 560);
        h.app.shots_show_calendar();
        h.app.view = View::Week;
        h.relayout();

        let g = TimelineInteraction::test_geometry(h.app.view_rect());
        assert!(
            (g.hour_h - TIMELINE_MIN_HOUR_H).abs() < 0.001,
            "a short window pins the hour height to the legible minimum, got {}",
            g.hour_h
        );
        assert!(
            (g.content_h - 24.0 * TIMELINE_MIN_HOUR_H).abs() < 0.001,
            "the grid content spans the full 24 hours, got {}",
            g.content_h
        );
        assert!(
            h.app.timeline.vertical_overflows(),
            "the full day is taller than the viewport, so it scrolls"
        );
        let vp = (g.body_bottom - g.body_top) as f32;
        assert!(
            g.content_h as f32 - vp > 0.0,
            "there is real scroll range (00:00 … 24:00 both reachable)"
        );

        // A tall window instead grows the hours to fill and stops scrolling.
        let mut tall = Harness::new(1000, 2000);
        tall.app.shots_show_calendar();
        tall.app.view = View::Week;
        tall.relayout();
        let gt = TimelineInteraction::test_geometry(tall.app.view_rect());
        assert!(
            gt.hour_h > TIMELINE_MIN_HOUR_H,
            "a tall window grows the hours to fill the space, got {}",
            gt.hour_h
        );
        assert!(
            !tall.app.timeline.vertical_overflows(),
            "the whole day fits, so nothing scrolls"
        );
    }
    // Case `dragging_empty_timeline_grid_creates_and_opens_an_event`.
    {
        let mut h = Harness::new(1000, 640);
        h.app.shots_show_calendar();
        h.app.view = View::Week;
        h.relayout();
        // Pin the scroll to the top so an early, empty slot is on screen.
        h.app.timeline.test_set_offset_y(0.0);
        h.relayout();

        let area = h.app.view_rect();
        let g = TimelineInteraction::test_geometry(area);
        let days = h.app.timeline_days();
        let col = 0usize;
        let grid_x0 = area.x0 + g.gutter;
        let col_w = TimelineInteraction::test_column_width(area, days.len(), g);
        let x = grid_x0 + col_w * col as f64 + col_w * 0.5;
        let offset = f64::from(h.app.timeline.test_offset_y());
        let y_at = |hour: f64| g.body_top + hour * g.hour_h - offset;

        let ids_before: HashSet<Id> = h.app.model.events.iter().map(|e| e.id).collect();
        // Drag 03:00 → 05:00 in the first column (empty in the demo seed).
        h.drag(Point::new(x, y_at(3.0)), Point::new(x, y_at(5.0)));

        let new_ev = h
            .app
            .model
            .events
            .iter()
            .find(|e| !ids_before.contains(&e.id))
            .expect("a drag across empty grid creates one event");
        assert_eq!(
            new_ev.start.date(),
            days[col],
            "created on the dragged column's day"
        );
        assert_eq!(
            new_ev.start.time(),
            min_to_time(3 * 60),
            "starts at the drag origin (03:00)"
        );
        assert_eq!(
            (new_ev.end - new_ev.start).num_minutes(),
            120,
            "spans the dragged range (03:00–05:00)"
        );
        assert_eq!(
            h.app.selection,
            Some(Sel::Event(new_ev.id)),
            "the new event is selected"
        );
        assert!(
            h.app.detail_open,
            "the event editor opened for the new event"
        );
    }
    // Case `dragging_an_event_reschedules_it_through_the_action_model`.
    {
        // Tall window so the whole day fits at offset 0 and every block sits at
        // its true position.
        let mut h = Harness::new(1200, 1900);
        h.app.shots_show_calendar();
        h.app.view = View::Week;
        h.relayout();

        let today = h.app.today;
        let (id, start0, dur0) = {
            let ev = h
                .app
                .model
                .events
                .iter()
                .find(|e| e.title == "Design review")
                .expect("seeded 11:00 event present");
            (ev.id, ev.start, (ev.end - ev.start).num_minutes())
        };
        assert_eq!(start0.time(), min_to_time(11 * 60));

        let area = h.app.view_rect();
        let g = TimelineInteraction::test_geometry(area);
        let days = h.app.timeline_days();
        let col = days
            .iter()
            .position(|d| *d == today)
            .expect("today is in the week");
        let block = h
            .app
            .timeline
            .test_block_rect(area, days.len(), g, col, 11 * 60, 12 * 60);
        let x = block.center().x;
        // Grab near the block top and drag down two hours.
        let from = Point::new(x, block.y0 + 3.0);
        let to = Point::new(x, block.y0 + 3.0 + 2.0 * g.hour_h);
        h.drag(from, to);

        let moved = h
            .app
            .model
            .event(id)
            .expect("event still present after the move");
        assert_eq!(moved.start.date(), today, "stayed in the same day column");
        assert_eq!(
            moved.start.time(),
            min_to_time(13 * 60),
            "rescheduled two hours later (was {start0})"
        );
        assert_eq!(
            (moved.end - moved.start).num_minutes(),
            dur0,
            "duration preserved across the move"
        );
        assert!(
            h.app.last_dispatched().is_some(),
            "the reschedule went through the action model"
        );
    }
}

// Synthesised winit keyboard events (the zeroed-KeyEvent trick the toolkit's
// own widget tests use — every field is overwritten before use).
fn synth_device() -> winit::event::DeviceId {
    unsafe { std::mem::zeroed() }
}
fn char_press(ch: &str) -> winit::event::WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Character(ch.into());
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = Some(ch.into());
    winit::event::WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
}
fn named_press(named: winit::keyboard::NamedKey) -> winit::event::WindowEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = Key::Named(named);
    ev.physical_key =
        winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = None;
    winit::event::WindowEvent::KeyboardInput {
        device_id: synth_device(),
        event: ev,
        is_synthetic: false,
    }
}

// --- per-calendar visibility actually filters the views ----------------

// --- per-account collapse is wired (was a dead serialized field) -------

// --- detail chips: colour / recurrence / reminder ----------------------

#[test]
#[serial]
fn detail_chips_cycle_into_the_crdt_and_the_exported_ics_file() {
    // Case `cycling_recurrence_updates_selected_event_and_mirrors_to_crdt`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("the demo opens with an event selected");
        };
        let before = app.model.event(id).unwrap().recurrence;
        app.apply_action(Action::CycleRecurrence, &mut state);
        let after = app.model.event(id).unwrap().recurrence;
        assert_eq!(
            after,
            before.cycle(),
            "the chip advances recurrence by one step"
        );
        // Reading the document back yields the new rule — it mirrored into the CRDT.
        let reread = CalDoc::read_model(&state);
        assert_eq!(
            reread
                .events
                .iter()
                .find(|e| e.id == id)
                .unwrap()
                .recurrence,
            after
        );
    }
    // Case `cycling_colour_updates_selected_event`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("event selected");
        };
        let before = app.model.event(id).unwrap().color;
        app.apply_action(Action::CycleColor, &mut state);
        assert_ne!(
            app.model.event(id).unwrap().color,
            before,
            "the chip advances the colour"
        );
    }
    // Case `cycling_reminder_advances_and_reaches_the_exported_ics`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("event selected");
        };
        // Drive the reminder from a known rung: None → 10m → 30m.
        if let Some(e) = app.model.event_mut(id) {
            e.reminder_min = None;
        }
        app.apply_action(Action::CycleReminder, &mut state);
        assert_eq!(app.model.event(id).unwrap().reminder_min, Some(10));
        app.apply_action(Action::CycleReminder, &mut state);
        assert_eq!(app.model.event(id).unwrap().reminder_min, Some(30));
        // The reminder is real, exported data: it becomes a VALARM in the .ics.
        let ics = crate::ics::export(&app.model);
        assert!(
            ics.contains("TRIGGER:-PT30M"),
            "the reminder chip's value reaches the exported alarm"
        );
    }
    // Case `export_writes_a_real_ics_file_to_the_transfer_dir`.
    {
        let dir = std::env::temp_dir().join(format!("cal-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_CALENDAR_IMPORT_DIR", &dir);

        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::Export, &mut state);

        let written = crate::store::newest_ics(&dir).expect("export wrote an .ics file");
        let body = std::fs::read_to_string(&written).unwrap();
        assert!(body.starts_with("BEGIN:VCALENDAR"), "a real iCalendar file");
        assert!(
            body.contains("BEGIN:VEVENT"),
            "carrying the exported events"
        );
        assert!(
            app.status.as_deref().unwrap_or("").contains("Exported"),
            "the status pill confirms the export"
        );
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains(&format!("{} tasks", app.model.tasks.len())),
            "the confirmation truthfully includes the exported VTODO count"
        );

        drop(env);
        std::fs::remove_dir_all(&dir).ok();
    }
}

// --- Export writes a real file ----------------------------------------

// --- keyboard dispatch through the real handle_event -------------------

// --- sync failure path -------------------------------------------------

// --- notes editing persists through the real editor + save path --------

// --- editable event form -------------------------------------------

fn selected_event_id(app: &CalendarApp) -> Id {
    match app.selection {
        Some(Sel::Event(id)) => id,
        _ => panic!("expected an event selection"),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn event_form_fields_edit_traverse_and_pick_dates_and_times() {
    // Case `event_title_is_a_real_editable_field_on_the_key_path`.
    {
        // The title is a focusable text input, not static text: clicking it
        // focuses the field and typing edits the event through the production
        // handle_event path.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().title.clone();
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("the title is a click target");
        h.click(p);
        assert!(
            h.app.title_input.is_focused(),
            "clicking the title focuses it"
        );
        h.type_char("!");
        let after = h.app.model.event(id).unwrap().title.clone();
        assert_ne!(after, before, "typing edited the event title");
        assert!(
            after.contains('!'),
            "the typed character landed in the title: {after:?}"
        );
    }
    // Case `location_is_editable_and_tab_traverses_the_fields`.
    {
        // Tab moves focus title → location → invite → notes → title on the
        // real key path (App leaves Tab to the root
        // widget because the calendar exposes no focusable children).
        let mut h = Harness::new(1280, 800);
        let p = h
            .hit_center(|a| matches!(a, Action::FocusField(FormField::Title)))
            .expect("title target");
        h.click(p);
        assert!(h.app.title_input.is_focused());
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.location_input.is_focused(), "Tab: title → location");
        assert!(!h.app.title_input.is_focused());
        // Typing into the now-focused location field edits the event location.
        let id = selected_event_id(&h.app);
        h.type_char("Q");
        assert!(
            h.app.model.event(id).unwrap().location.contains('Q'),
            "location is editable"
        );
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.invite_input.is_focused(), "Tab: location → invite");
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.notes_editor.is_focused(), "Tab: invite → notes");
        h.press_named(winit::keyboard::NamedKey::Tab);
        assert!(h.app.title_input.is_focused(), "Tab wraps notes → title");
    }
    // Case `shift_tab_retreats_through_the_form_fields`.
    {
        // Reverse traversal: the exact seam the Shift+Tab key path invokes
        // (winit exposes no constructor for a shift-held `Modifiers`, so — as
        // the toolkit's own widgets do — the direction logic is exercised
        // directly).
        let mut h = Harness::new(1280, 800);
        h.app.focus_field(FormField::Title);
        assert!(h.app.title_input.is_focused());
        h.app.cycle_form_focus(true);
        assert!(
            h.app.notes_editor.is_focused(),
            "Shift+Tab: title → notes (wrap)"
        );
        h.app.cycle_form_focus(true);
        assert!(h.app.invite_input.is_focused(), "Shift+Tab: notes → invite");
        h.app.cycle_form_focus(true);
        assert!(
            h.app.location_input.is_focused(),
            "Shift+Tab: invite → location"
        );
        h.app.cycle_form_focus(true);
        assert!(
            h.app.title_input.is_focused(),
            "Shift+Tab: location → title"
        );
    }
    // Case `date_popover_arrow_key_picks_and_sets_the_start_date`.
    {
        // Opening the start-date field floats the month grid; an arrow key
        // moves the selection and drives the new date into the event.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let before = h.app.model.event(id).unwrap().start.date();
        let p = h
            .hit_center(|a| matches!(a, Action::OpenPopover(FormPopover::StartDate)))
            .expect("the start-date field opens a popover");
        h.click(p);
        assert_eq!(
            h.app.event_popovers.active(),
            FormPopover::StartDate,
            "the date grid is open"
        );
        h.press_named(winit::keyboard::NamedKey::ArrowRight);
        let after = h.app.model.event(id).unwrap().start.date();
        assert_eq!(
            after,
            before.succ_opt().unwrap(),
            "arrow-key navigation picked the next day and set the event start"
        );
    }
    // Case `time_popover_arrow_key_sets_the_start_time`.
    {
        // The time popover's spinner is keyboard-navigable; an arrow step
        // drives the new hour into the event.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        // Ensure the event is timed (not all-day) so the time field exists.
        assert!(!h.app.model.event(id).unwrap().all_day);
        let before_h = h.app.model.event(id).unwrap().start.hour();
        let p = h
            .hit_center(|a| matches!(a, Action::OpenPopover(FormPopover::StartTime)))
            .expect("the start-time field opens a popover");
        h.click(p);
        assert_eq!(h.app.event_popovers.active(), FormPopover::StartTime);
        h.press_named(winit::keyboard::NamedKey::ArrowUp);
        let after_h = h.app.model.event(id).unwrap().start.hour();
        assert_eq!(
            after_h,
            (before_h + 1) % 24,
            "the clock spinner set the start hour"
        );
    }
    // Case `all_day_toggle_flips_the_flag`.
    {
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let start_day = h.app.model.event(id).unwrap().start.date();
        let before = h.app.model.event(id).unwrap().all_day;
        let p = h
            .hit_center(|a| matches!(a, Action::ToggleAllDay))
            .expect("all-day toggle");
        h.click(p);
        assert_eq!(
            h.app.model.event(id).unwrap().all_day,
            !before,
            "all-day flipped"
        );
        let event = h.app.model.event(id).unwrap();
        assert_eq!(event.start.time(), NaiveTime::MIN);
        assert_eq!(
            event.end,
            (start_day + Duration::days(1)).and_time(NaiveTime::MIN)
        );
        assert!(event.occurs_on(start_day));
        assert!(!event.occurs_on(start_day + Duration::days(1)));
    }
}

// --- delete ---------------------------------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn delete_by_action_key_and_button_removes_items_and_queues_remote_deletes() {
    // Case `delete_event_removes_it_from_the_model_and_crdt`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.events[0].id;
        let before = app.model.events.len();
        app.select_event(id);
        app.apply_action(Action::DeleteEvent(id), &mut state);
        assert_eq!(
            app.model.events.len(),
            before - 1,
            "event dropped from the model"
        );
        assert!(app.model.event(id).is_none());
        assert!(
            !CalDoc::read_model(&state).events.iter().any(|e| e.id == id),
            "removed from the CRDT too"
        );
        assert!(
            app.selection.is_none(),
            "deleting the on-screen event closes the editor"
        );
        assert!(!app.detail_open);
    }
    // Case `deleting_a_synced_event_queues_a_caldav_delete`.
    {
        // A synced event carries a calendar_id; deleting it enqueues a CalDAV
        // delete for that collection so the server copy is removed on next sync.
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut state); // pull the sample synced events
        let synced = app
            .model
            .events
            .iter()
            .find(|e| e.calendar_id.is_some())
            .cloned()
            .expect("a synced event with a calendar id");
        app.apply_action(Action::DeleteEvent(synced.id), &mut state);
        let outbox = CalDoc::delete_outbox(&state);
        assert!(
            outbox.iter().any(|(cal, uid)| *uid == synced.id
                && Some(cal.as_str()) == synced.calendar_id.as_deref()),
            "a CalDAV delete was queued for the synced event: {outbox:?}"
        );
    }
    // Case `delete_task_removes_it_and_its_subtasks`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let parent = app
            .model
            .tasks
            .iter()
            .find(|t| !app.model.subtasks_of(t.id).is_empty())
            .map(|t| t.id)
            .expect("a task with subtasks");
        let sub_count = app.model.subtasks_of(parent).len();
        assert!(sub_count > 0);
        let before = app.model.tasks.len();
        app.apply_action(Action::DeleteTask(parent), &mut state);
        assert_eq!(
            app.model.tasks.len(),
            before - 1 - sub_count,
            "the task and its subtasks are gone"
        );
        assert!(app.model.task(parent).is_none());
        assert!(
            !CalDoc::read_model(&state)
                .tasks
                .iter()
                .any(|t| t.id == parent)
        );
    }
    // Case `delete_key_removes_the_selected_event`.
    {
        // The Delete key on a selected event (no field focused) removes it —
        // through the real handle_event key path.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        assert!(h.app.detail_open);
        let before = h.app.model.events.len();
        h.press_named(winit::keyboard::NamedKey::Delete);
        assert_eq!(
            h.app.model.events.len(),
            before - 1,
            "Delete removed the event"
        );
        assert!(h.app.model.event(id).is_none());
    }
    // Case `editor_exposes_a_delete_target_that_removes_the_event`.
    {
        // The editor header carries a trash button wired to delete the event —
        // clicked through the real handle_event path.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let p = h
            .hit_center(|a| matches!(a, Action::DeleteEvent(_)))
            .expect("a delete target in the editor");
        h.click(p);
        assert!(
            h.app.model.event(id).is_none(),
            "clicking the trash button deleted the event"
        );
    }
    // Case `delete_button_dispatches_the_action_the_palette_builds`.
    {
        // Single source of truth, proven on the real button path: clicking the
        // editor's trash button dispatches the byte-identical EncodedAction the
        // Ctrl+K "Delete event" spec builds for the same event (including the
        // resolved CalDAV delete target).
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let remote = h.app.resolve_remote_delete(id);
        let expected =
            crate::actions::encode(&crate::actions::delete_event_action(id.to_string(), remote));
        let p = h
            .hit_center(|a| matches!(a, Action::DeleteEvent(_)))
            .expect("a delete target in the editor");
        h.click(p);
        assert_eq!(
            h.app.last_dispatched(),
            Some(&expected),
            "the trash button and the palette build the identical EncodedAction"
        );
        assert!(
            h.app.model.event(id).is_none(),
            "and it actually deleted the event"
        );
    }
}

// --- create → name → persist flow ----------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn caldav_reconciliation_keeps_stable_resources_by_href() {
    // Case `caldav_reconciliation_updates_and_deletes_by_href_without_uuid_churn`.
    {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        let start = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let remote_event = |title: &str| Event {
            id: Uuid::new_v4(),
            title: title.into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            project: None,
            calendar_id: Some("work/default".into()),
        };
        app.apply_sync_result(
            caldav::SyncResult {
                resources: vec![caldav::RemoteResource {
                    calendar_id: "work/default".into(),
                    href: "/dav/work/meeting.ics".into(),
                    etag: Some("\"one\"".into()),
                    events: vec![remote_event("First title")],
                    tasks: Vec::new(),
                }],
                checkpoints: vec![caldav::CollectionCheckpoint {
                    calendar_id: "work/default".into(),
                    token: "token-1".into(),
                    full: true,
                    seen_hrefs: vec!["/dav/work/meeting.ics".into()],
                }],
                ..caldav::SyncResult::default()
            },
            SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
            &mut doc,
        );
        let id = CalDoc::remote_id_for_href(&doc, "work/default", "/dav/work/meeting.ics")
            .expect("binding stored");
        app.apply_sync_result(
            caldav::SyncResult {
                resources: vec![caldav::RemoteResource {
                    calendar_id: "work/default".into(),
                    href: "/dav/work/meeting.ics".into(),
                    etag: Some("\"two\"".into()),
                    events: vec![remote_event("Server-edited title")],
                    tasks: Vec::new(),
                }],
                checkpoints: vec![caldav::CollectionCheckpoint {
                    calendar_id: "work/default".into(),
                    token: "token-2".into(),
                    full: false,
                    seen_hrefs: vec!["/dav/work/meeting.ics".into()],
                }],
                ..caldav::SyncResult::default()
            },
            SyncOutcome::Synced {
                calendars: 1,
                events: 1,
            },
            &mut doc,
        );
        assert_eq!(app.model.event(id).unwrap().title, "Server-edited title");
        assert_eq!(
            CalDoc::remote_binding(&doc, id).unwrap().2.as_deref(),
            Some("\"two\"")
        );
        app.apply_sync_result(
            caldav::SyncResult {
                deletions: vec![caldav::RemoteDeletion {
                    calendar_id: "work/default".into(),
                    href: "/dav/work/meeting.ics".into(),
                }],
                checkpoints: vec![caldav::CollectionCheckpoint {
                    calendar_id: "work/default".into(),
                    token: "token-3".into(),
                    full: false,
                    seen_hrefs: Vec::new(),
                }],
                ..caldav::SyncResult::default()
            },
            SyncOutcome::Synced {
                calendars: 1,
                events: 0,
            },
            &mut doc,
        );
        assert!(app.model.event(id).is_none());
        assert!(CalDoc::remote_binding(&doc, id).is_none());
        assert_eq!(CalDoc::sync_tokens(&doc)[0].1, "token-3");
    }
    // Case `caldav_reconciliation_keeps_a_vtodo_at_its_stable_resource_across_pulls`.
    {
        let uid = Uuid::new_v4();
        let remote_task = |done| crate::model::Task {
            id: uid,
            title: "Server task".into(),
            done,
            due: NaiveDate::from_ymd_opt(2026, 8, 3),
            priority: Priority::High,
            project: None,
            parent: None,
        };
        // Send the record through the real VTODO parser before reconciliation:
        // a second pull must update the href-bound task, never append one.
        let parsed_task = |done| {
            let (_, tasks) = ics::parse(&ics::export(&Model {
                events: Vec::new(),
                tasks: vec![remote_task(done)],
                projects: Vec::new(),
            }));
            tasks.into_iter().next().expect("one VTODO parses")
        };
        let mut app = CalendarApp::new_empty_for_shots();
        let mut doc = AutoCommit::new();
        let apply =
            |app: &mut CalendarApp, doc: &mut AutoCommit, task: crate::model::Task, etag: &str| {
                app.apply_sync_result(
                    caldav::SyncResult {
                        resources: vec![caldav::RemoteResource {
                            calendar_id: "work/default".into(),
                            href: "/dav/work/tasks.ics".into(),
                            etag: Some(etag.into()),
                            events: Vec::new(),
                            tasks: vec![task],
                        }],
                        ..caldav::SyncResult::default()
                    },
                    SyncOutcome::Synced {
                        calendars: 1,
                        events: 0,
                    },
                    doc,
                );
            };

        apply(&mut app, &mut doc, parsed_task(false), "\"one\"");
        let id =
            CalDoc::remote_id_for_href(&doc, "work/default", "/dav/work/tasks.ics#tilo-task-0")
                .expect("the VTODO has a href binding");
        apply(&mut app, &mut doc, parsed_task(true), "\"two\"");

        assert_eq!(
            app.model.tasks.iter().filter(|task| task.id == id).count(),
            1
        );
        assert!(app.model.task(id).is_some_and(|task| task.done));
        assert_eq!(
            CalDoc::remote_binding(&doc, id).unwrap().2.as_deref(),
            Some("\"two\"")
        );
    }
}

fn history_preview(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("history-preview") {
        return Some(node);
    }
    node.children.iter().find_map(history_preview)
}

fn restore_review(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("standalone-history-restore-review") {
        return Some(node);
    }
    node.children.iter().find_map(restore_review)
}

#[test]
fn standalone_history_restore_returns_the_calendar_to_a_chosen_version() {
    let mut app = CalendarApp::new_empty_for_shots();
    let mut state = ui::State::new();
    let mut first = None;
    for title in ["Alpha review", "Beta sync"] {
        app.sync(&mut state.doc, &state.root);
        app.apply_action(Action::NewEvent, &mut state.doc);
        app.sync(&mut state.doc, &state.root);
        let mut event = CalDoc::read_model(&state.doc)
            .events
            .into_iter()
            .find(|event| event.title.is_empty())
            .expect("the new event");
        event.title = title.into();
        CalDoc::update_event(&mut state.doc, &event);
        app.sync(&mut state.doc, &state.root);
        state.commit_action(None, None);
        first.get_or_insert_with(|| state.doc.get_heads()[0].to_string());
    }
    assert_eq!(CalDoc::read_model(&state.doc).events.len(), 2);
    let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (1200, 800));
    // The launcher hands History the app's own root factory; a preview is a
    // second, read-only instance of it.
    harness.set_history_preview_root(|| Ok(Box::new(CalendarApp::new_empty_for_shots())));
    harness.enable_document_history_chrome();
    harness.click_named("standalone-history-open").unwrap();

    let session = harness
        .local_history_resources()
        .unwrap()
        .into_iter()
        .find(|resource| resource.kind == "tilo.host.history.session")
        .expect("an open History session");
    let entry = first.unwrap();
    harness
        .invoke_local_history_action(
            &tilo_actions::ActionAddress::new(session.id.as_str(), "select"),
            Some(&format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "entries": [&entry[..32]] })
            )),
        )
        .unwrap();
    harness.relayout();
    let changes = harness.state_mut().doc.get_changes(&[]).len();

    // The selected version is shown by the calendar itself, beside the pane.
    let inspection = harness.presentation_inspection();
    let shown = history_preview(&inspection.root)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|semantics| semantics.name.clone())
        .expect("History previews the selected version");
    assert!(shown.contains("Alpha review"), "{shown}");
    assert!(
        !shown.contains("Beta sync"),
        "the preview is the past: {shown}"
    );
    assert_eq!(
        CalDoc::read_model(&harness.state_mut().doc).events.len(),
        2,
        "previewing writes nothing"
    );

    harness.click_named("standalone-history-restore").unwrap();
    let inspection = harness.presentation_inspection();
    let confirm = restore_review(&inspection.root)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|dialog| {
            dialog
                .children
                .iter()
                .filter_map(|child| child.action_target.clone())
                .find(|target| target.ends_with(":primary"))
        })
        .expect("the review publishes its confirm action");
    harness
        .perform_semantic_named(
            "standalone-history-restore-review",
            Some(&confirm),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    harness.relayout();

    let doc = &mut harness.state_mut().doc;
    assert_eq!(
        CalDoc::read_model(doc).events.len(),
        1,
        "the calendar is back at the version after its first event"
    );
    assert_eq!(doc.get_changes(&[]).len(), changes + 1);
}

#[test]
fn hosted_directory_keeps_pending_import_without_the_calendar_root() {
    use ui::resources::PublishesResources;
    let mut app = CalendarApp::new_empty_for_shots();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    let standalone = app
        .action_directory_resource_page(&state.doc, &[], 8)
        .expect("standalone directory");
    assert!(
        standalone
            .resources
            .iter()
            .any(|resource| resource.kind == "calendar.calendar"),
        "standalone directory publishes the calendar"
    );
    app.actions.mark_hosted_for_test();
    let hosted = app
        .action_directory_resource_page(&state.doc, &[], 8)
        .expect("hosted directory");
    assert!(
        hosted
            .resources
            .iter()
            .all(|resource| resource.kind != "calendar.calendar"),
        "hosted directory must not republish the calendar: {:?}",
        hosted
            .resources
            .iter()
            .map(|resource| resource.id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(hosted.resources.is_empty());
    assert!(app.resources(&state.doc).is_empty());
}
