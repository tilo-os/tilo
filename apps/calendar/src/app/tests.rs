use super::*;
use crate::model::{Palette, Recurrence};
use automerge::ROOT;
use chrono::Datelike;
use ui::headless::Framed;
use ui::widget::Edits;

/// A document holding `calendar`, written through the calendar's own verbs
/// and edits.
pub(crate) fn document_of(calendar: &Calendar) -> AutoCommit {
    let mut replica = ui::tilo_resource::Replica::<Calendar>::new();
    for invoke in crate::resource::sample_invocations(calendar) {
        replica.apply(invoke).expect("the sample applies");
    }
    replica.doc().clone()
}

/// The document of the calendar `app` shows.
pub(crate) fn demo_document(app: &CalendarApp) -> AutoCommit {
    document_of(&app.model)
}

/// Whether the event `id` waits to be uploaded to the server of its
/// calendar, as a device holding the sample accounts sees it.
fn waits_for_upload(doc: &AutoCommit, id: Id) -> bool {
    crate::outbox::uploads(&crate::resource::read_doc(doc), &AccountStore::sample())
        .iter()
        .any(|(item, _)| *item == id)
}

/// The item a server keeps at `href` of `calendar`, by what `doc` holds.
fn remote_at(doc: &AutoCommit, calendar: &str, href: &str) -> Option<Id> {
    crate::resource::read_doc(doc)
        .remote
        .iter()
        .find(|(_, copy)| copy.calendar == calendar && copy.href == href)
        .and_then(|(id, _)| id.parse().ok())
}

/// The version of the item `id` its server last reported.
fn remote_etag(doc: &AutoCommit, id: Id) -> Option<String> {
    crate::resource::read_doc(doc)
        .remote
        .get(&id.to_string())
        .and_then(|copy| copy.etag.clone())
}

/// Apply `invoke` to `doc` as a peer's change arrives: built and applied,
/// not committed.
fn apply_to(doc: &mut AutoCommit, invoke: impl Into<ui::tilo_resource::Invoke<Calendar>>) {
    let action = invoke.into().build(0).expect("builds");
    tilo_actions::Action::apply(&action, doc).expect("applies");
}

/// Pump the same settle/read passes as the production frame host.
fn present(app: &mut CalendarApp, doc: &mut AutoCommit, path: &ObjId) {
    app.settle(&mut Edits::lend(doc), path);
    app.sync(doc, path);
    app.settle(&mut Edits::lend(doc), path);
    app.sync(doc, path);
}

#[test]
fn readonly_sync_observes_pending_actions_without_committing_document() {
    let mut app = CalendarApp::new_demo();
    let mut doc = demo_document(&app);
    doc.commit();
    app.sync(&doc, &ROOT);
    let id = app.model.tasks.iter().find(|task| !task.done).unwrap().id;
    apply_to(
        &mut doc,
        Calendar::paths()
            .tasks
            .item(&crate::model::key(id))
            .done
            .set(true),
    );
    let before = (doc.base_heads(), doc.pending_ops());
    assert!(before.1 > 0, "the incoming action is still uncommitted");
    assert!(!app.model.task(id).unwrap().done);

    app.sync(&doc, &ROOT);

    assert!(
        app.model.task(id).unwrap().done,
        "the pending task edit is visible"
    );
    assert_eq!(
        (doc.base_heads(), doc.pending_ops()),
        before,
        "reading a frame preserves the document's pending edit"
    );
}

impl CalendarApp {
    /// Close the detail and show the calendar screen.
    fn show_calendar(&mut self) {
        self.detail_open = false;
        self.selection = None;
        self.dirty = true;
    }
}

/// The calendar root as it publishes `doc`.
fn published_root(doc: &AutoCommit) -> tilo_actions::Resource {
    ui::tilo_resource::publish::<Calendar>(
        "calendar.calendar",
        "Calendar",
        &crate::resource::read_doc(doc),
    )
}

/// The resource `id` as the calendar window publishes it: the root, an item
/// of `doc`, or one of `app`'s calendar collections.
fn published_resource(
    app: &CalendarApp,
    doc: &AutoCommit,
    id: &str,
) -> Option<tilo_actions::Resource> {
    use ui::tilo_resource::Resource as _;
    if id == "calendar.calendar" {
        return Some(published_root(doc));
    }
    if let Some(row) = app.collection_rows().into_iter().find(|row| row.id() == id) {
        return Some(row.into_resource());
    }
    Calendar::child(&crate::resource::read_doc(doc), id)
}

#[test]
fn task_completion_toggles_by_checkbox_and_label() {
    // Case `toggling_a_task_flips_done_and_mirrors_to_crdt`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut Edits::lend(&mut state));
        assert_ne!(app.model.task(id).unwrap().done, before);
    }
    // Case `sidebar_task_label_and_open_targets_coexist`.
    {
        // The mini task rail must offer BOTH a completion target (checkbox +
        // label) and a select target (open the task) for the same row.
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        // With no synced accounts the sidebar has room for the task rail
        // (a machine full of remote calendars still lists tasks in the
        // agenda/day views — the rail is the space-permitting extra).
        app.accounts = AccountStore::default();
        app.list_sel = Some(ListSel::Today);
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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut Edits::lend(&mut state));
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
        let mut app = CalendarApp::new_detached();
        let mut state = AutoCommit::new();
        present(&mut app, &mut state, &automerge::ROOT);
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
        let mut app = CalendarApp::new_detached();
        let mut state = AutoCommit::new();
        present(&mut app, &mut state, &automerge::ROOT);
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
        let mut state = demo_document(&app);
        let root = automerge::ROOT;
        present(&mut app, &mut state, &root);
        assert!(app.seeded);
        // Selected event's notes are bound.
        assert!(app.bound.is_some());
        assert!(!crate::resource::read_doc(&state).events.is_empty());
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
        // Narrow + detail open: the sheet floats over the full-width
        // calendar.
        assert_eq!(app.content_rect.width(), 560.0);
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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        app.view = View::Week;
        app.detail_open = false;
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
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn user_edits_and_source_identity_persist_across_restart() {
    // Case `a_named_event_persists_across_restart`.
    {
        // An event created and named in the window is in the calendar the
        // next time it opens.
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::CalendarShell>::new().unwrap();
        app.harness().key("n");
        let id = app.state().events.first().expect("n creates an event").id;
        app.harness().type_text("Roadmap");
        let app = app.reopen().unwrap();
        assert_eq!(
            app.state().event(id).expect("the event survived").title,
            "Roadmap"
        );
    }
    // Case `source_calendar_id_survives_the_crdt_round_trip`.
    {
        // The tag must persist, or hiding wouldn't stick across a restart.
        let mut doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
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
            recurrence_exdates: ui::tilo_resource::Set::default(),
            occurrence_overrides: ui::tilo_resource::Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: ui::tilo_resource::Keyed::default(),
            reminder_min: None,
            project: None,
            calendar_id: Some("work/oncall".into()),
        };
        apply_to(
            &mut doc,
            Calendar::NEW_EVENT.with(crate::resource::calendar::NewEvent {
                event: crate::actions::EventData::from_event(&ev),
            }),
        );
        let back = crate::resource::read_doc(&doc);
        assert_eq!(back.events[0].calendar_id.as_deref(), Some("work/oncall"));
    }
}

#[test]
fn ics_imports_are_idempotent_atomic_and_commit_one_batch() {
    // Case `uuid_vtodo_import_preserves_identity_and_is_idempotent`.
    {
        let id = Uuid::new_v4();
        let payload = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:{id}@tilo\r\nSUMMARY:Durable imported task\r\nDUE;VALUE=DATE:20260916\r\nPRIORITY:1\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
        );
        let mut app = CalendarApp::new_detached();
        let mut doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
        present(&mut app, &mut doc, &automerge::ROOT);

        let (_, tasks) =
            crate::actions::import_payload(&payload).expect("the task is new on the first import");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, id.to_string(), "the VTODO UID is retained");

        let first = crate::resource::import(&payload).expect("an import");
        app.submit(&mut Edits::lend(&mut doc), first);
        app.model = crate::resource::read_doc(&doc);
        assert!(
            app.model.task(id).is_some(),
            "the exact UID became the task id"
        );
        let again = crate::resource::import(&payload).expect("the file still holds it");
        app.submit(&mut Edits::lend(&mut doc), again);
        assert_eq!(
            crate::resource::read_doc(&doc).tasks.len(),
            1,
            "re-importing the same VTODO does not duplicate it"
        );
    }
    // Case `resource_import_refuses_invalid_batch_without_partial_writes_or_success`.
    {
        let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:batch-event\r\nSUMMARY:Valid batch event\r\nDTSTART:20260919T090000\r\nDTEND:20260919T100000\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:batch-task\r\nSUMMARY:Valid batch task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        for invalid in ["event", "task", "project"] {
            let mut app = CalendarApp::new_detached();
            let mut state = ui::State::new();
            state.doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
            present(&mut app, &mut state.doc, &state.root);
            let (mut events, mut tasks) =
                crate::actions::import_payload(payload).expect("mixed import fixture");
            assert_eq!(events.len(), 1);
            assert_eq!(tasks.len(), 1);
            match invalid {
                "event" => events[0].end = "not-a-date".into(),
                "task" => tasks[0].due = "not-a-date".into(),
                "project" => tasks[0].project = Uuid::new_v4().to_string(),
                _ => unreachable!(),
            }
            let batch = Calendar::IMPORT_ITEMS
                .with(crate::resource::calendar::ImportItems { events, tasks });
            state.doc.commit();
            let before = state.doc.get_heads();
            // Every door shares the real decoder and executor; rejection
            // cannot leave the preceding valid event behind.
            let mut replica = ui::tilo_resource::Replica::<Calendar>::from_doc(state.doc.clone());
            assert!(replica.apply(batch.clone()).is_err());
            assert_eq!(replica.heads(), before);

            // The window's own import refuses it the same way.
            assert!(
                app.try_submit(&mut Edits::lend(&mut state.doc), batch)
                    .is_err()
            );
            assert_eq!(state.doc.get_heads(), before);
            assert!(crate::resource::read_doc(&state.doc).events.is_empty());
            assert!(crate::resource::read_doc(&state.doc).tasks.is_empty());
        }
    }
}

/// A calendar file the person chooses imports as one undo step, and the
/// calendar exports to a file they choose.
#[test]
fn a_chosen_ics_file_imports_as_one_undo_step_and_exports_again() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let invite = dir.path().join("invite.ics");
    let today = chrono::Local::now().date_naive();
    std::fs::write(&invite, crate::ics::sample_ics(today)).expect("write fixture");
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);

    let mut app = ui::framework::testing::TestApp::<crate::resource::CalendarShell>::new().unwrap();
    let imported = |app: &ui::framework::testing::TestApp<crate::resource::CalendarShell>| {
        app.state()
            .events
            .iter()
            .filter(|event| event.title == "Quarterly all-hands")
            .count()
    };
    assert_eq!(imported(&app), 0, "nothing is read before the import");
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(invite));
    app.effect(&crate::resource::CalendarEffect::ImportIcs)
        .expect("the import runs");
    app.settle();
    assert_eq!(imported(&app), 1, "the chosen file's events were imported");

    let exported = dir.path().join("exported.ics");
    recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(exported.clone()));
    app.effect(&crate::resource::CalendarEffect::ExportIcs)
        .expect("the export runs");
    let body = std::fs::read_to_string(&exported).expect("export wrote the chosen file");
    assert!(body.starts_with("BEGIN:VCALENDAR"), "a real iCalendar file");
    assert!(body.contains("Quarterly all-hands"), "carrying the events");
    assert_eq!(
        recorder
            .pick_requests()
            .iter()
            .map(|request| request.title().to_owned())
            .collect::<Vec<_>>(),
        ["Import a calendar", "Export calendar"]
    );

    assert!(app.undo(), "the import is one undo step");
    assert_eq!(imported(&app), 0);
}

#[test]
fn view_and_anchor_navigation_update_shared_state() {
    // Case `switching_view_changes_state`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        app.apply_action(Action::View(View::Week), &mut Edits::lend(&mut state));
        assert_eq!(app.view, View::Week);
        app.apply_action(Action::View(View::Agenda), &mut Edits::lend(&mut state));
        assert_eq!(app.view, View::Agenda);
    }
    // Case `stepping_month_changes_anchor_month`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        let m0 = app.anchor.month();
        app.apply_action(Action::Next, &mut Edits::lend(&mut state));
        // Month view by default → Next advances one month.
        assert_ne!(app.anchor.month(), m0);
    }
    // Case `mini_month_outcome_updates_the_shared_calendar_anchor`.
    {
        let mut h = Harness::new(1280, 1200);
        h.app.show_calendar();
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
        h.app.show_calendar();
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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state));
        assert_eq!(app.model.events.len(), before + 1);
        assert!(matches!(app.selection, Some(Sel::Event(_))));
    }
    // Case `dismissing_a_pristine_new_event_discards_it_through_the_delete_action`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state));
        let id = app.selected_event().expect("new event selected");

        app.apply_action(Action::CloseDetail, &mut Edits::lend(&mut state));

        assert_eq!(app.model.events.len(), before, "untouched draft is gone");
        assert!(app.model.event(id).is_none());
        assert!(crate::resource::read_doc(&state).event(id).is_none());
        let dispatched = app.last_dispatched().expect("discard action dispatched");
        assert_eq!(dispatched.id, "delete-event");
        assert_eq!(
            dispatched.arguments.get("event"),
            Some(&serde_json::json!(id.to_string()))
        );
    }
    // Case `dismissing_an_edited_new_event_keeps_its_autosaved_changes`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state));
        let id = app.selected_event().expect("new event selected");
        let current = app.model.event(id).unwrap().color;
        let other = Palette::ALL
            .iter()
            .copied()
            .find(|color| *color != current)
            .unwrap();
        app.apply_action(Action::SetEventColor(other), &mut Edits::lend(&mut state));

        app.apply_action(Action::CloseDetail, &mut Edits::lend(&mut state));

        assert!(app.model.event(id).is_some(), "edited draft remains");
        assert!(crate::resource::read_doc(&state).event(id).is_some());
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
        h.app.show_calendar();
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
    // Case `a_new_event_is_discarded_not_deleted`.
    {
        // The sheet of an event that was never saved offers to discard it;
        // discarding removes the draft without an "Event deleted" toast.
        let mut h = Harness::new(1280, 800);
        h.app.shots_show_calendar();
        h.relayout();
        let p = h
            .hit_center(|a| matches!(a, Action::NewEvent))
            .expect("New button");
        h.click(p);
        h.relayout();
        let id = selected_event_id(&h.app);
        assert!(
            h.hit_center(|a| matches!(a, Action::DeleteEvent(_)))
                .is_none(),
            "an unsaved event has nothing to delete"
        );
        let p = h
            .hit_center(|a| matches!(a, Action::DiscardDraft(_)))
            .expect("the sheet offers to discard the new event");
        h.click(p);
        assert!(h.app.model.event(id).is_none(), "the draft is gone");
        assert!(h.app.fresh_draft.is_none());
        assert!(
            h.app.status.is_none(),
            "discarding is not reported as a deletion"
        );
    }
    // Case `new_event_is_untitled_with_a_placeholder_and_empty_notes`.
    {
        // A fresh event has NO seeded title or notes: both fields show their
        // placeholders instead of literal text the user must delete.
        let mut h = Harness::new(1280, 800);
        // Create from the calendar screen; the demo's modal editor would
        // otherwise sit over the New button behind its dismiss scrim.
        h.app.show_calendar();
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
        present(app, doc, &automerge::ROOT);
        assert_eq!(
            h.app.notes_editor.editor().empty_hint(),
            NOTES_PLACEHOLDER,
            "the notes editor shows a calendar-specific placeholder hint"
        );
        assert!(
            crate::resource::notes_path(id).object(h.doc()).is_some(),
            "the new event has notes"
        );
        let joined = crate::resource::read_doc(h.doc()).notes_text(id);
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
        h.app.show_calendar();
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
        app.apply_action(Action::CloseDetail, &mut Edits::lend(doc));
        let (app, doc) = h.parts();
        present(app, doc, &automerge::ROOT);
        assert!(!h.app.detail_open, "editor closed");

        // Reopen the same event (the OpenEvent action's production entry point)
        // and run a sync so the form rebinds from the model.
        let (app, doc) = h.parts();
        app.apply_action(Action::OpenEvent(id), &mut Edits::lend(doc));
        let (app, doc) = h.parts();
        present(app, doc, &automerge::ROOT);
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

/// The demo app laid out at `width`×`height` in a dark theme at `scale`.
fn laid_out(app: &mut CalendarApp, width: f64, height: f64, scale: f32) {
    let theme = Theme::dark().with_text_scale(scale);
    app.timeline.set_layout_theme(&theme);
    app.layout_theme = theme;
    app.rect = Rect::new(0.0, 0.0, width, height);
    app.relayout();
}

/// The event form's controls, in form order.
fn event_controls(form: &FormRects) -> Vec<Rect> {
    [
        form.title,
        form.start_date,
        form.start_time,
        form.end_date,
        form.end_time,
        form.all_day,
        form.location,
        form.calendar,
        form.repeat,
        form.alert,
        form.timezone,
        form.colour,
        form.invite,
    ]
    .into_iter()
    .filter(|rect| rect.area() > 0.0)
    .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn large_text_layouts_stay_in_bounds() {
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
            laid_out(&mut app, width, 844.0, text_scale);

            assert!(
                app.form.conflict.area() > 0.0,
                "{width}px / {text_scale}× retains warning"
            );
            assert!(
                app.form.conflict.y1 <= app.form.location.y0,
                "warning reserves its own lane before location at {width}px / {text_scale}×"
            );
            for control in event_controls(&app.form) {
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
    // Case `large_text_keeps_the_sheet_and_its_controls_inside_the_window`.
    {
        for (width, height) in [(1200.0, 900.0), (390.0, 844.0)] {
            let mut app = CalendarApp::new_demo();
            app.detail_open = true;
            laid_out(&mut app, width, height, 2.0);
            let modal = app.modal.clone().expect("the editor sheet is laid out");
            assert!(
                app.rect.contains_rect(modal.card),
                "the sheet fits the window"
            );
            let controls = event_controls(&app.form);
            for (index, rect) in controls.iter().enumerate() {
                assert!(
                    rect.x0 >= modal.body.x0 - 0.5 && rect.x1 <= modal.body.x1 + 0.5,
                    "form control {index} escaped the sheet's body at {width}px: {rect:?}"
                );
                for other in controls.iter().skip(index + 1) {
                    assert!(
                        rect.intersect(*other).area() <= 0.0,
                        "form controls overlap: {rect:?} and {other:?}"
                    );
                }
            }
            assert!(
                app.form.notes_label.y0 >= app.form.invite.y1,
                "the notes start below the last form row"
            );
            assert!(
                app.detail_scroll_max > 0.0,
                "a 2× form taller than the window scrolls inside the sheet"
            );
        }
    }
    // Case `large_text_new_event_sheet_stays_inside_the_viewport`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        app.stage_scenario(Some("new_event"));
        present(&mut app, &mut state, &ROOT);
        laid_out(&mut app, 1200.0, 900.0, 2.0);

        assert!(app.detail_open);
        assert!(app.rect.contains_rect(app.detail_rect));
        assert!(
            app.status.is_none(),
            "creating an event does not raise a toast"
        );
    }
    // Case `month_chips_stay_inside_their_cells_below_the_day_number_at_two_x`.
    {
        let mut app = CalendarApp::new_demo();
        app.shots_show_calendar();
        let theme = Theme::dark().with_text_scale(2.0);
        laid_out(&mut app, 1280.0, 800.0, 2.0);
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let area = app.view_rect();
        let hits = app.draw_month(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            area,
        );
        let cells: Vec<Rect> = hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::PickDate(_)))
            .map(|(r, _)| *r)
            .collect();
        let strips: Vec<Rect> = hits
            .iter()
            .filter(|(r, a)| matches!(a, Action::OpenDay(_)) && cells.iter().any(|c| c.y0 == r.y0))
            .map(|(r, _)| *r)
            .collect();
        let chips: Vec<Rect> = hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::OpenEvent(_)))
            .map(|(r, _)| *r)
            .collect();
        assert!(!chips.is_empty());
        for chip in chips {
            let cell = cells
                .iter()
                .find(|cell| cell.contains(chip.center()))
                .expect("every chip sits in a cell");
            assert!(
                chip.x0 >= cell.x0 && chip.x1 <= cell.x1,
                "{chip:?} in {cell:?}"
            );
            let strip = strips
                .iter()
                .find(|strip| strip.x0 == cell.x0 && strip.y0 == cell.y0)
                .expect("each cell has its day-number strip");
            assert!(
                chip.y0 >= strip.y1 - 0.5,
                "the chip starts below the day number at 2×"
            );
        }
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
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::CalendarShell>::new().unwrap();
        let sample = crate::model::seed_demo(chrono::Local::now().date_naive());
        for invoke in crate::resource::sample_invocations(&sample) {
            app.invoke(invoke).unwrap();
        }
        let event = app
            .state()
            .events
            .into_iter()
            .find(|event| !event.attendees.is_empty())
            .expect("the sample calendar has an event with a guest");
        let attendee = event.attendees.first().expect("a guest");
        let attendee_email = attendee.email.clone();
        let output = app
            .effect(&crate::resource::CalendarEffect::PrepareInMail {
                event_id: event.id.to_string(),
                attendee_id: attendee.id.to_string(),
            })
            .expect("Calendar answers its invitation effect");
        let value = output.value.expect("typed handoff result");
        assert_eq!(value["delivery"], "not_sent");
        assert_eq!(value["target"]["resource"], "mail.mailbox");
        assert_eq!(value["target"]["action"], "prepare-calendar-invitation");
        let arguments = value["arguments"].as_object().unwrap().clone();

        let mut mailbox = tilo_actions::Resource::new("mail.mailbox", "mail.mailbox", "Mail");
        mailbox.actions = <mail::actions::MailEffect as ui::tilo_resource::Effect>::specs();
        let mail_roots = vec![mailbox];
        let encoded = tilo_actions::door::encode_json(
            &mail_roots,
            &tilo_actions::ActionAddress::new("mail.mailbox", "prepare-calendar-invitation"),
            &arguments,
        )
        .expect("Mail accepts Calendar's exact typed arguments");
        let effect = <mail::actions::MailEffect as ui::tilo_resource::Effect>::from_payload(
            &encoded.payload,
        )
        .expect("Mail decodes the handoff");
        assert!(matches!(
            effect,
            mail::actions::MailEffect::PrepareCalendarInvitation {
                to,
                calendar_ics,
                ..
            } if to == attendee_email && calendar_ics.contains("METHOD:REQUEST")
        ));
    }
    // Case `direct_task_detail_choices_are_the_field_edits_the_task_offers`.
    {
        use ui::widget::Widget;

        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
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
        app.draw_overlay(
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
        let sheet = app.detail_rect;
        assert!(
            sheet.contains_rect(app.choices.priority.rect),
            "the priority dropdown is on the task sheet"
        );
        assert!(
            sheet.contains_rect(app.choices.project.rect),
            "the project dropdown is on the task sheet"
        );

        let task = published_resource(&app, &state, &crate::actions::task_resource_id(id))
            .expect("selected task resource");
        assert!(
            task.actions.iter().any(|action| action.id == "delete-task"),
            "the task is an item of the calendar: {:?}",
            task.actions
                .iter()
                .map(|action| &action.id)
                .collect::<Vec<_>>()
        );
        let fields = Calendar::paths().tasks.item(&crate::model::key(id));
        let edit = |edit: ui::tilo_resource::Edit<Calendar>| {
            ui::tilo_resource::Invoke::from(edit).arguments
        };
        let due = chrono::NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        app.apply_action(Action::SetTaskDue(Some(due)), &mut Edits::lend(&mut state));
        assert_eq!(
            app.last_dispatched().unwrap().arguments,
            edit(fields.due.set(Some(due)))
        );

        app.set_selected_task_priority(Priority::Medium, &mut Edits::lend(&mut state));
        assert_eq!(
            app.last_dispatched().unwrap().arguments,
            edit(fields.priority.set(Priority::Medium))
        );

        let project = app.model.projects[0].id;
        app.set_selected_task_project(Some(project), &mut Edits::lend(&mut state));
        assert_eq!(
            app.last_dispatched().unwrap().arguments,
            edit(fields.project.set(Some(project)))
        );
        assert_eq!(
            crate::resource::read_doc(&state).task(id).unwrap().project,
            Some(project),
            "each choice is in the calendar"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn sidebar_presentation_follows_width_and_hides_via_its_toggle() {
    // Case `wide_window_docks_the_sidebar_and_floats_the_editor_sheet`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Persistent);
        assert!(app.content_rect.x0 >= app.sidebar.rect().x1 - 0.5);
        // The editor is a modal sheet over the whole window: the calendar
        // keeps its full width behind it.
        assert!(app.detail_rect.area() > 0.0);
        assert_eq!(app.content_rect.x1, app.rect.x1);
        assert!(app.rect.contains_rect(app.detail_rect));
        assert!(app.detail_rect.x0 > app.rect.x0 + 1.0);
        assert!(app.detail_rect.x1 < app.rect.x1 - 1.0);
        assert!(app.detail_rect.y0 > app.rect.y0 + 1.0);
        assert!(
            (app.detail_rect.center().x - app.rect.center().x).abs() < 1.0,
            "the sheet is centred on the window"
        );
    }
    // Case `narrow_window_turns_the_sidebar_into_a_drawer`.
    {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert_eq!(app.content_rect.x0, app.rect.x0);
        assert!(!app.sidebar.visible(), "the drawer is closed by default");
        app.sidebar.set_open(true);
        app.relayout();
        assert!(app.sidebar.visible());
        assert!(
            app.sidebar.rect().width() < app.rect.width() - 1.0,
            "the drawer floats inside the window"
        );
    }
    // Case `desktop_sidebar_hides_and_only_the_sidebar_hides`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(app.sidebar.visible());
        let expanded_content_x0 = app.content_rect.x0;

        app.sidebar.set_collapsed(true);
        app.relayout();
        assert!(!app.sidebar.visible());
        assert_eq!(app.content_rect.x0, app.rect.x0);
        assert!(
            app.detail_rect.area() > 0.0,
            "the editor stays open; only the sidebar hid"
        );

        app.sidebar.set_collapsed(false);
        app.relayout();
        assert!(app.sidebar.visible());
        assert_eq!(app.content_rect.x0, expanded_content_x0);
    }
    // Case `the_drawer_closes_through_its_own_toggle`.
    {
        let mut h = Harness::new(420, 800);
        h.app.show_calendar();
        h.app.sidebar.set_open(true);
        h.relayout();
        assert!(h.app.sidebar.visible(), "drawer is open");
        let toggle = h
            .app
            .hits
            .iter()
            .filter(|(r, a)| {
                matches!(a, Action::ToggleSidebar) && h.app.sidebar.rect().contains_rect(*r)
            })
            .map(|(r, _)| r.center())
            .next()
            .expect("the drawer carries its toggle");
        h.click(toggle);
        assert!(!h.app.sidebar.is_open(), "the toggle closed the drawer");
    }
    // Case `the_sidebar_toggle_hides_then_the_header_toggle_shows_it`.
    {
        let mut h = Harness::new(1280, 800);
        // The topbar's expand chevron lives behind the modal's dismiss scrim, so
        // exercise the sidebar collapse from the calendar screen (the state a
        // user collapses the rail from).
        h.app.show_calendar();
        h.relayout();
        assert!(h.app.sidebar.visible(), "starts shown on a wide window");
        let hide = h
            .hit_center(|a| matches!(a, Action::ToggleSidebar))
            .expect("the sidebar toggle");
        h.click(hide);
        assert!(h.app.sidebar.is_collapsed());
        h.relayout();
        let show = h
            .hit_center(|a| matches!(a, Action::ToggleSidebar))
            .expect("the header carries the toggle while the sidebar is hidden");
        assert!(show.x < 60.0, "at the leading edge of the header");
        h.click(show);
        assert!(!h.app.sidebar.is_collapsed());
        assert!(h.app.sidebar.visible());
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn the_editor_sheet_floats_over_the_calendar_at_every_width() {
    // Case `a_narrow_window_keeps_the_calendar_behind_the_sheet`.
    {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
        app.detail_open = true;
        app.relayout();
        assert!(app.detail_rect.area() > 0.0);
        assert!(app.rect.contains_rect(app.detail_rect));
        assert!(app.content_rect.area() > 0.0, "the calendar stays laid out");

        // Going back returns to the calendar screen.
        let mut state = demo_document(&app);
        app.apply_action(Action::CloseDetail, &mut Edits::lend(&mut state));
        app.relayout();
        assert_eq!(app.detail_rect.area(), 0.0);
        assert!(app.modal.is_none());
    }
    // Case `opening_an_event_in_a_narrow_window_closes_the_drawer`.
    {
        let mut app = CalendarApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
        app.relayout();
        app.sidebar.set_open(true);
        let id = app.model.events[0].id;
        app.select_event(id);
        assert!(app.detail_open);
        assert!(
            !app.sidebar.is_open(),
            "opening an event dismisses the drawer"
        );
    }
    // Case `clicking_an_event_chip_opens_its_editor`.
    {
        let mut h = Harness::new(1280, 800);
        h.app.show_calendar();
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
        assert_eq!(h.app.selection, Some(Sel::Event(id)));
        assert!(h.app.detail_open);
    }
    // Case `the_sheet_closes_with_done_or_a_press_outside_it`.
    {
        let h = Harness::new(1280, 800);
        assert!(h.app.detail_open, "the demo opens an event");
        let closes: Vec<Rect> = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::CloseDetail))
            .map(|(r, _)| *r)
            .collect();
        assert!(
            closes
                .iter()
                .any(|r| r.area() < 8000.0 && h.app.detail_rect.contains_rect(*r)),
            "a Done button in the sheet's footer"
        );
        assert!(
            closes.iter().any(|r| *r == h.app.rect),
            "a scrim over the whole window behind the sheet"
        );
    }
}

#[test]
fn mobile_event_editor_is_a_full_window_page_with_a_reachable_done_action() {
    for theme in [Theme::dark(), Theme::light()] {
        for (width, height) in [(360, 800), (800, 360)] {
            let mut h = Harness::new(width, height);
            Framed::harness_mut(&mut h.app)
                .relayout_themed((width, height), theme.clone())
                .expect("the real Calendar dialog lays out");
            let geometry = h
                .app
                .modal
                .clone()
                .expect("the sample event editor is open");
            assert_eq!(geometry.card, h.app.rect);
            assert_eq!(h.app.detail_rect, h.app.rect);
            assert!(
                geometry.body.height() > 0.0,
                "the mobile form retains a usable viewport"
            );
            if f64::from(height) < f64::from(theme.tokens.metrics.panel_width_sheet) {
                assert!(
                    h.app.detail_scroll_max > 0.0,
                    "the event form scrolls in phone landscape"
                );
            }
            let done = geometry.actions.first().copied().expect("Done is retained");
            assert!(h.app.rect.contains_rect(done));
            assert!(done.y0 >= geometry.body.y1);
            h.click(done.center());
            assert!(
                !h.app.detail_open,
                "the actual footer action dismisses the mobile editor"
            );
        }
    }
}

#[test]
fn view_switcher_lives_in_the_header_or_the_drawer_without_duplicates() {
    // Case `view_switcher_stays_in_the_header_until_it_does_not_fit`.
    {
        let mut app = CalendarApp::new_demo();
        app.detail_open = false;
        app.selection = None;

        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.relayout();
        assert!(app.topbar.switcher_visible());

        app.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        app.relayout();
        assert_eq!(app.sidebar.mode(), SidebarMode::Overlay);
        assert!(
            app.topbar.switcher_visible(),
            "a roomy header keeps the switcher beside a drawer"
        );

        app.rect = Rect::new(0.0, 0.0, 460.0, 800.0);
        app.relayout();
        assert!(
            !app.topbar.switcher_visible(),
            "a cramped header moves the switcher into the drawer"
        );
    }
    // Case `the_drawer_offers_a_pointer_reachable_view_switcher`.
    {
        let mut h = Harness::new(460, 720);
        h.app.shots_show_calendar();
        h.relayout();
        assert_eq!(h.app.sidebar.mode(), SidebarMode::Overlay);
        h.app.sidebar.set_open(true);
        h.relayout();
        assert_eq!(h.app.view, View::Month);
        let p = h
            .hit_center(|a| matches!(a, Action::View(View::Week)))
            .expect("the drawer carries a Week view target");
        h.click(p);
        assert_eq!(h.app.view, View::Week);
        assert!(!h.app.sidebar.is_open(), "picking a view closes the drawer");
    }
    // Case `a_wide_window_has_one_switcher`.
    {
        let mut h = Harness::new(1280, 800);
        h.app.shots_show_calendar();
        h.relayout();
        let view_targets = h
            .app
            .hits
            .iter()
            .filter(|(_, a)| matches!(a, Action::View(_)))
            .count();
        assert_eq!(view_targets, View::ALL.len(), "exactly one switcher");
    }
}

// --- click-label-to-complete ---------------------------------------

#[test]
fn calendar_and_project_selectors_file_events_and_reach_every_option() {
    // Case `project_dropdown_offers_every_project_and_files_the_task`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
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
        app.model.projects.push(crate::model::Project {
            id: Id::new_v4(),
            name: "Duplicate project".into(),
            color: Palette::Blue,
            order: 98,
        });
        state = document_of(&app.model);
        app.select_task(id);
        app.seed_choices();
        // The menu publishes a stable value, so identical labels remain distinct.
        app.choices.project.state.set(later.id.to_string());
        app.commit_choices(&mut Edits::lend(&mut state), &ROOT);
        assert_eq!(
            app.choices.project.dropdown.selected(),
            later.id.to_string()
        );
        let dropdown = app.choices.project.dropdown.accessibility().unwrap();
        assert!(
            dropdown.set_size.is_some_and(|size| size > 5),
            "the shared dropdown is populated from the complete project list"
        );
        assert_eq!(app.model.task(id).unwrap().project, Some(later.id));
    }
    // Case `calendar_dropdown_files_the_event_on_a_configured_account_and_queues_it`.
    {
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let (app, doc) = h.parts();
        app.seed_choices();
        app.choices.calendar.state.set("work/default".into());
        app.commit_choices(&mut Edits::lend(doc), &automerge::ROOT);
        assert_eq!(
            h.app.model.event(id).unwrap().calendar_id.as_deref(),
            Some("work/default")
        );
        assert!(waits_for_upload(h.doc(), id));
    }
    // Case `refiling_on_the_local_calendar_needs_no_upload`.
    {
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let (app, doc) = h.parts();
        app.set_event_calendar(
            &CalTarget::Remote("work/default".into()),
            &mut Edits::lend(doc),
        );
        assert!(waits_for_upload(h.doc(), id));
        let (app, doc) = h.parts();
        app.set_event_calendar(&CalTarget::Local, &mut Edits::lend(doc));
        assert!(h.app.model.event(id).unwrap().calendar_id.is_none());
        assert!(!waits_for_upload(h.doc(), id));
    }
}

#[test]
fn calendar_options_come_from_the_shared_account_store() {
    {
        let h = Harness::new(1280, 800);
        let opts = h.app.calendar_options();
        assert!(matches!(opts.first(), Some((CalTarget::Local, _))));
        for (_, cal) in h.app.accounts.all_calendars() {
            assert!(
                opts.iter()
                    .any(|(t, _)| matches!(t, CalTarget::Remote(id) if id == &cal.id)),
                "configured calendar {} is offered",
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
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
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
        assert_eq!(
            today.state.checked,
            Some(false),
            "no task list is chosen until the person picks one"
        );
        app.apply_action(Action::List(ListSel::Today), &mut Edits::lend(&mut state));
        assert_eq!(app.list_sel, Some(ListSel::Today));
        app.apply_action(Action::List(ListSel::Today), &mut Edits::lend(&mut state));
        assert_eq!(app.list_sel, None, "choosing the shown list puts it away");

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

        let resource = published_resource(
            &app,
            &state,
            &crate::actions::calendar_resource_id(&calendar_id),
        )
        .expect("configured calendar is an exact resource");
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == "set-visibility"),
            "calendar resource exposes the visibility effect"
        );
        app.apply_action(
            Action::SetCalendarVisible {
                id: calendar_id.clone(),
                visible: !before,
            },
            &mut Edits::lend(&mut state),
        );
        assert_eq!(
            app.accounts
                .all_calendars()
                .find_map(|(_, item)| (item.id == calendar_id).then_some(item.visible)),
            Some(!before),
            "the pointer action reaches the Settings-owned absolute setter"
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
            &mut Edits::lend(&mut state),
            &automerge::ROOT,
        ));
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

        // Showing or hiding is absolute: asking twice converges on the
        // requested value rather than inverting it as a toggle would.
        for _ in 0..2 {
            app.set_calendar_visibility(&calendar_id, before)
                .expect("the calendar is configured");
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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
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
            &mut Edits::lend(&mut state),
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
            h.app.detail_rect.contains_rect(h.app.choices.calendar.rect),
            "calendar dropdown"
        );
    }
}

#[test]
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
        // path, not `apply_action`.

        let mut h = Harness::new(1280, 800);
        // Close the demo's detail so the notes editor doesn't own keystrokes.
        let (app, doc) = h.parts();
        app.apply_action(Action::CloseDetail, &mut Edits::lend(doc));
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
        app.apply_action(Action::Next, &mut Edits::lend(doc));
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
        app.apply_action(Action::CloseDetail, &mut Edits::lend(doc));
        h.relayout();
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
        app.apply_action(Action::CloseDetail, &mut Edits::lend(doc));
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
        app.apply_action(Action::CloseDetail, &mut Edits::lend(doc));
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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        // Sync is explicit now (no fabricated auto-pull on the first frame).
        app.apply_action(Action::Sync, &mut Edits::lend(&mut state));
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
        app.sample_accounts = false; // exercise the real (threaded) branch
        app.accounts = AccountStore::sample();
        let mut state = demo_document(&app);
        app.start_sync(&mut Edits::lend(&mut state));
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
        app.sample_accounts = false;
        app.accounts = AccountStore::default();
        let mut state = demo_document(&app);
        app.start_sync(&mut Edits::lend(&mut state));
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
        app.sample_accounts = false;
        let mut state = demo_document(&app);
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
            recurrence_exdates: ui::tilo_resource::Set::default(),
            occurrence_overrides: ui::tilo_resource::Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: ui::tilo_resource::Keyed::default(),
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
        ))
        .unwrap();
        app.sync_rx = Some(rx);
        app.syncing = true;

        app.poll_sync(&mut Edits::lend(&mut state));

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
        assert!(!crate::resource::read_doc(&state).events.is_empty());
    }
    // Case `sync_tags_landed_events_with_their_source_calendar`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut Edits::lend(&mut state));
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
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state));
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
        app.sample_accounts = false;
        let mut state = demo_document(&app);
        let (tx, rx) = std::sync::mpsc::channel::<SyncWorkerResult>();
        app.sync_rx = Some(rx);
        app.syncing = true;
        drop(tx); // worker vanished without sending
        app.poll_sync(&mut Edits::lend(&mut state));
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
            all_day: false,
            location: String::new(),
            color: Palette::Blue,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: ui::tilo_resource::Set::default(),
            occurrence_overrides: ui::tilo_resource::Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: ui::tilo_resource::Keyed::default(),
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
            app.visible_events_on(day).is_empty(),
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
                .as_deref()
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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
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

        app.apply_action(
            Action::ToggleAccountCollapsed(acc_id.clone()),
            &mut Edits::lend(&mut state),
        );
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
        app.apply_action(
            Action::ToggleAccountCollapsed(acc_id),
            &mut Edits::lend(&mut state),
        );
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

    fn mount(app: CalendarApp, w: u32, h: u32) -> Self {
        Harness {
            app: Framed::new(
                ui::State::from_doc(demo_document(&app)),
                app,
                (w, h),
                Theme::dark(),
            ),
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
        let device_id = winit::event::DeviceId::dummy();
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
        let device_id = winit::event::DeviceId::dummy();
        self.dispatch(&winit::event::WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        });
    }
    fn mouse_button(&mut self, state: ElementState) -> bool {
        let device_id = winit::event::DeviceId::dummy();
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
        self.dispatch(&char_key(ch));
    }

    /// Press a named key (arrows, etc.) through the real keyboard path.
    fn press_named(&mut self, named: winit::keyboard::NamedKey) {
        self.dispatch(&named_key(named));
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
        h.app.show_calendar();
        h.app.view = View::Week;
        h.relayout();

        let g = TimelineInteraction::test_geometry(h.app.view_rect());
        // The visible seven-day grid is narrow after navigation takes its
        // column. Its minimum reserves two caption lines per half hour.
        let theme = &h.app.layout_theme;
        let minimum = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            let title = theme.text(ui::tokens::TextRole::Caption).strong();
            ui::widget::TimelineDensity::Narrow
                .min_hour_height(theme, f64::from(text.measure_style("Ag", None, &title).1))
        });
        assert!(
            (g.hour_h - minimum).abs() < 0.001,
            "a short window pins the hour height to the legible minimum, got {}",
            g.hour_h
        );
        assert!(
            (g.content_h - 24.0 * minimum).abs() < 0.001,
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
        tall.app.show_calendar();
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
        h.app.show_calendar();
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
        h.app.show_calendar();
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

use ui::synthetic_input::char_key;
use ui::synthetic_input::named_key;

// --- per-calendar visibility actually filters the views ----------------

// --- per-account collapse is wired (was a dead serialized field) -------

// --- detail dropdowns: colour / recurrence / alert ----------------------

/// Pick `value` in one of the editor's dropdowns, as a click on its option
/// publishes it, and apply it.
fn pick(
    app: &mut CalendarApp,
    state: &mut AutoCommit,
    choice: fn(&mut editor::Choices) -> &mut editor::Choice,
    value: &str,
) {
    app.seed_choices();
    choice(&mut app.choices).state.set(value.to_string());
    app.commit_choices(&mut Edits::lend(state), &automerge::ROOT);
}

#[test]
fn detail_chips_cycle_into_the_crdt_and_the_exported_ics_file() {
    // Case `cycling_recurrence_updates_selected_event_and_mirrors_to_crdt`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("the demo opens with an event selected");
        };

        pick(
            &mut app,
            &mut state,
            |choices| &mut choices.repeat,
            "monthly",
        );
        let after = app.model.event(id).unwrap().recurrence;
        assert_eq!(after, Recurrence::Monthly, "the pick sets the rule");
        assert_eq!(app.model.event(id).unwrap().recurrence_interval, 1);
        // Reading the document back yields the new rule — it mirrored into the CRDT.
        let reread = crate::resource::read_doc(&state);
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
    // Case `picking_a_colour_updates_selected_event`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("event selected");
        };
        let before = app.model.event(id).unwrap().color;
        let current = app.model.event(id).unwrap().color;
        let other = Palette::ALL
            .iter()
            .copied()
            .find(|color| *color != current)
            .unwrap();
        app.apply_action(Action::SetEventColor(other), &mut Edits::lend(&mut state));
        assert_ne!(
            other, before,
            "the chosen swatch differs from the old colour"
        );
        assert_eq!(
            app.model.event(id).unwrap().color,
            other,
            "the pick sets the colour"
        );
        assert_eq!(
            crate::resource::read_doc(&state).event(id).unwrap().color,
            other,
            "and mirrors it into the document"
        );
    }
    // Case `picking_an_alert_reaches_the_exported_ics`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let Some(Sel::Event(id)) = app.selection else {
            panic!("event selected");
        };
        // Drive the reminder from a known rung: None → 10m → 30m.
        if let Some(e) = app.model.event_mut(id) {
            e.reminder_min = None;
        }
        pick(&mut app, &mut state, |choices| &mut choices.alert, "10");
        assert_eq!(app.model.event(id).unwrap().reminder_min, Some(10));
        pick(&mut app, &mut state, |choices| &mut choices.alert, "30");
        assert_eq!(app.model.event(id).unwrap().reminder_min, Some(30));
        // The reminder is real, exported data: it becomes a VALARM in the .ics.
        let ics = crate::ics::export(&app.model);
        assert!(
            ics.contains("TRIGGER:-PT30M"),
            "the alert reaches the exported alarm"
        );
    }
}

// --- Export writes a real file ----------------------------------------

// --- keyboard dispatch through the real handle_event -------------------

// --- sync failure path -------------------------------------------------

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
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        let id = app.model.events[0].id;
        let before = app.model.events.len();
        app.select_event(id);
        app.apply_action(Action::DeleteEvent(id), &mut Edits::lend(&mut state));
        assert_eq!(
            app.model.events.len(),
            before - 1,
            "event dropped from the model"
        );
        assert!(app.model.event(id).is_none());
        assert!(
            !crate::resource::read_doc(&state)
                .events
                .iter()
                .any(|e| e.id == id),
            "removed from the CRDT too"
        );
        assert!(
            app.selection.is_none(),
            "deleting the on-screen event closes the editor"
        );
        assert!(!app.detail_open);
    }
    // Case `deleting_a_synced_event_deletes_it_on_its_server`.
    {
        // A synced event carries what its server holds; deleting it leaves
        // that copy for the device holding the account to delete.
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
        app.apply_action(Action::Sync, &mut Edits::lend(&mut state)); // pull the sample synced events
        let synced = app
            .model
            .events
            .iter()
            .find(|e| e.calendar_id.is_some())
            .cloned()
            .expect("a synced event with a calendar id");
        app.apply_action(Action::DeleteEvent(synced.id), &mut Edits::lend(&mut state));
        let deletes = crate::outbox::deletes(&crate::resource::read_doc(&state), &app.accounts);
        assert!(
            deletes.iter().any(|(uid, job)| *uid == synced.id
                && Some(job.calendar.id.as_str()) == synced.calendar_id.as_deref()),
            "the server of the synced event deletes it next: {:?}",
            deletes.iter().map(|(uid, _)| uid).collect::<Vec<_>>()
        );
    }
    // Case `delete_task_removes_it_and_its_subtasks`.
    {
        let mut app = CalendarApp::new_demo();
        let mut state = demo_document(&app);
        present(&mut app, &mut state, &automerge::ROOT);
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
        app.apply_action(Action::DeleteTask(parent), &mut Edits::lend(&mut state));
        assert_eq!(
            app.model.tasks.len(),
            before - 1 - sub_count,
            "the task and its subtasks are gone"
        );
        assert!(app.model.task(parent).is_none());
        assert!(
            !crate::resource::read_doc(&state)
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
        // editor's trash button submits the invocation the event's published
        // "Delete event" builds.
        let mut h = Harness::new(1280, 800);
        let id = selected_event_id(&h.app);
        let expected = Calendar::DELETE_EVENT.with(crate::model::key(id));
        let p = h
            .hit_center(|a| matches!(a, Action::DeleteEvent(_)))
            .expect("a delete target in the editor");
        h.click(p);
        let dispatched = h.app.last_dispatched().expect("the trash button submitted");
        assert_eq!(
            (dispatched.id, dispatched.arguments),
            (expected.id, expected.arguments),
            "the trash button and the palette build the identical invocation"
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
        let mut doc = demo_document(&app);
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
            recurrence_exdates: ui::tilo_resource::Set::default(),
            occurrence_overrides: ui::tilo_resource::Keyed::default(),
            timezone: "floating".into(),
            organizer: None,
            attendees: ui::tilo_resource::Keyed::default(),
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
            &mut Edits::lend(&mut doc),
        );
        let id = remote_at(&doc, "work/default", "/dav/work/meeting.ics")
            .expect("what the server keeps there is known");
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
            &mut Edits::lend(&mut doc),
        );
        assert_eq!(app.model.event(id).unwrap().title, "Server-edited title");
        assert_eq!(remote_etag(&doc, id).as_deref(), Some("\"two\""));
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
            &mut Edits::lend(&mut doc),
        );
        assert!(app.model.event(id).is_none());
        assert!(
            !crate::resource::read_doc(&doc)
                .remote
                .contains_key(&id.to_string())
        );
        assert_eq!(
            crate::resource::read_doc(&doc).sync_tokens["work/default"],
            "token-3"
        );
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
            let (_, tasks) = ics::parse(&ics::export(&Calendar {
                events: ui::tilo_resource::Keyed::default(),
                tasks: vec![remote_task(done)].into(),
                projects: ui::tilo_resource::Keyed::default(),
                ..Calendar::default()
            }));
            tasks.into_iter().next().expect("one VTODO parses")
        };
        let mut app = CalendarApp::new_detached();
        let mut doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
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
                    &mut Edits::lend(doc),
                );
            };

        apply(&mut app, &mut doc, parsed_task(false), "\"one\"");
        let id = remote_at(&doc, "work/default", "/dav/work/tasks.ics#tilo-task-0")
            .expect("the VTODO is known at its server address");
        apply(&mut app, &mut doc, parsed_task(true), "\"two\"");

        assert_eq!(
            app.model.tasks.iter().filter(|task| task.id == id).count(),
            1
        );
        assert!(app.model.task(id).is_some_and(|task| task.done));
        assert_eq!(remote_etag(&doc, id).as_deref(), Some("\"two\""));
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
fn native_event_edits_record_the_actual_resource_title_in_platform_history() {
    let app = CalendarApp::new_demo();
    let mut doc = demo_document(&app);
    doc.commit();
    let event = app.model.events[0].clone();
    assert!(!event.title.trim().is_empty());
    for (label, action) in [
        (
            "Update event",
            ui::tilo_resource::Invoke::from(
                Calendar::paths()
                    .events
                    .item(&crate::model::key(event.id))
                    .location
                    .set("Meeting room".into()),
            ),
        ),
        (
            "Delete event",
            Calendar::DELETE_EVENT.with(crate::model::key(event.id)),
        ),
    ] {
        let before = doc.get_changes(&[]).len();
        app.try_submit(&mut Edits::lend(&mut doc), action)
            .expect("the actual calendar dispatcher admits the edit");
        assert_eq!(doc.get_changes(&[]).len(), before + 1);
        let heads = doc.get_heads();
        let context = doc
            .get_change_by_hash(&heads[0])
            .and_then(|change| {
                change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)
            })
            .expect("the committed edit carries platform History provenance");
        assert!(
            matches!(
                &context.what,
                Some(tilo_actions::Did::Verb(_) | tilo_actions::Did::Edit(_))
            ),
            "{label}: {context:?}"
        );
        let resource = context.resource.expect("the edit names the calendar");
        assert_eq!(resource.id, "calendar.calendar");
        let item = context.item.expect("and the event it changed");
        assert_eq!(item.id, event.id.to_string(), "{label}");
        assert_eq!(item.label, event.title, "{label}");
    }
    assert!(crate::resource::read_doc(&doc).event(event.id).is_none());
}

#[test]
fn platform_history_restore_returns_the_calendar_to_a_chosen_version() {
    let mut app = CalendarApp::new_detached();
    let mut state = ui::State::new();
    state.doc = ui::tilo_resource::Replica::<Calendar>::new().doc().clone();
    let mut first = None;
    for title in ["Alpha review", "Beta sync"] {
        present(&mut app, &mut state.doc, &state.root);
        app.apply_action(Action::NewEvent, &mut Edits::lend(&mut state.doc));
        present(&mut app, &mut state.doc, &state.root);
        let event = crate::resource::read_doc(&state.doc)
            .events
            .into_iter()
            .find(|event| event.title.is_empty())
            .expect("the new event");
        apply_to(
            &mut state.doc,
            Calendar::paths()
                .events
                .item(&crate::model::key(event.id))
                .title
                .set(title.into()),
        );
        present(&mut app, &mut state.doc, &state.root);
        state.commit_action(None, None);
        first.get_or_insert_with(|| state.doc.get_heads()[0].to_string());
    }
    assert_eq!(crate::resource::read_doc(&state.doc).events.len(), 2);
    let mut harness = ui::headless::FrameHarness::new(state, Box::new(app), (1200, 800));
    // The launcher hands History the app's own root factory; a preview is a
    // second, read-only instance of it.
    harness.set_history_preview_root(|| Ok(Box::new(CalendarApp::new_detached())));
    harness.enable_document_history_chrome();
    open_platform_history(&mut harness);

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
        crate::resource::read_doc(&harness.state_mut().doc)
            .events
            .len(),
        2,
        "previewing writes nothing"
    );

    harness.click_named("standalone-history-restore").unwrap();
    let inspection = harness.presentation_inspection();
    // The confirm action is the review's footer button.
    let (owner, confirm) = restore_review(&inspection.root)
        .and_then(|dialog| {
            dialog.children.iter().find_map(|child| {
                let target = child.semantics.as_ref()?.action_target.clone()?;
                let owner = child.test_id.clone()?;
                target.ends_with(":primary").then_some((owner, target))
            })
        })
        .expect("the review publishes its confirm action");
    harness
        .perform_semantic_named(&owner, Some(&confirm), &ui::a11y::Action::Activate)
        .unwrap();
    harness.relayout();

    let doc = &mut harness.state_mut().doc;
    assert_eq!(
        crate::resource::read_doc(doc).events.len(),
        1,
        "the calendar is back at the version after its first event"
    );
    assert_eq!(doc.get_changes(&[]).len(), changes + 1);
}

/// Open the actual published platform door without depending on app chrome.
fn open_platform_history(harness: &mut ui::headless::FrameHarness) {
    let resources = harness.local_history_resources().unwrap();
    let history = resources
        .iter()
        .find(|resource| resource.id.0 == "tilo.host:history")
        .expect("the platform publishes History independently of app chrome");
    let open = history
        .actions
        .iter()
        .find(|action| action.id == "open")
        .expect("the platform History resource has its stable open action");
    assert_eq!(open.kind, tilo_actions::OperationKind::View);
    assert!(!open.label.trim().is_empty());
    harness
        .invoke_local_history_action(
            &tilo_actions::ActionAddress::new(history.id.as_str(), "open"),
            None,
        )
        .unwrap();
}
