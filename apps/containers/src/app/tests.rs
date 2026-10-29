use super::*;

/// The sample engine, staged as a plain `--shots` opens it.
fn staged() -> ContainersApp {
    let mut app = ContainersApp::sample();
    app.stage_scene(None);
    app
}

fn driver(app: ContainersApp) -> ui::headless::FrameHarness {
    ui::headless::FrameHarness::for_root(ui::State::new(), app, (1280, 800), ui::Theme::dark())
}

fn app_of(driver: &mut ui::headless::FrameHarness) -> ui::headless::RootMut<'_, ContainersApp> {
    driver.root_mut::<ContainersApp>()
}

/// A finished verb says so in the window's bar, not over the list.
#[test]
fn a_result_is_the_window_bars_notice() {
    let mut d = driver(staged());
    app_of(&mut d).state().show_toast("Stopped web", false);
    d.relayout();
    let notice = ui::window_toolbar::notice().expect("the bar shows the result");
    assert_eq!(notice.text, "Stopped web");
    assert_eq!(notice.kind, ToastKind::Success);
    app_of(&mut d)
        .state()
        .show_toast("Podman isn't running", true);
    d.relayout();
    assert_eq!(
        ui::window_toolbar::notice().map(|notice| (notice.text, notice.kind)),
        Some(("Podman isn't running".to_owned(), ToastKind::Error)),
        "a newer result replaces the older one"
    );
}

#[test]
fn keys_and_buttons_move_between_sections_and_open_dialogs() {
    let mut d = driver(staged());
    let first = view::listed_containers(&app_of(&mut d).state().engine)[0]
        .id
        .clone();
    assert_eq!(
        app_of(&mut d).selected().as_deref(),
        Some(first.as_str()),
        "a demo opens on the first container"
    );

    d.key("2");
    assert_eq!(app_of(&mut d).section(), Section::Images);
    d.click_named("images-pull").unwrap();
    assert_eq!(
        app_of(&mut d).state().prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::PullImage)
    );
    d.key("Escape");
    assert!(
        app_of(&mut d).state().prompt.is_none(),
        "Escape cancels the prompt"
    );

    d.key("3");
    assert_eq!(app_of(&mut d).section(), Section::Volumes);
    d.click_named("volumes-new").unwrap();
    assert_eq!(
        app_of(&mut d).state().prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::NewVolume)
    );
    d.type_text("cache");
    d.key("Enter");
    assert!(app_of(&mut d).state().prompt.is_none());
    assert!(
        app_of(&mut d)
            .state()
            .engine
            .volumes
            .iter()
            .any(|volume| volume.name == "cache"),
        "the typed name became a volume"
    );

    d.key("1");
    assert_eq!(app_of(&mut d).section(), Section::Containers);
    app_of(&mut d).state().select_container(Some(first));
    d.relayout();
    d.key("Escape");
    assert!(
        app_of(&mut d).selected().is_none(),
        "Escape returns from the detail to the list"
    );
}

#[test]
fn the_detail_closes_from_its_header_control() {
    let mut d = driver(staged());
    assert!(app_of(&mut d).selected().is_some());
    d.click_named("detail-close").unwrap();
    assert!(
        app_of(&mut d).selected().is_none(),
        "the close control clears the chosen container"
    );
    d.relayout();
    assert!(
        !d.targets().contains_key("detail-close"),
        "with nothing chosen there is nothing to close"
    );
}

#[test]
fn the_section_and_its_commands_are_in_the_window_bar() {
    let mut d = driver(staged());
    d.present_in_window_bar();
    d.relayout();
    let bar = ui::window_toolbar::declared().expect("the shell declares the window's slots");
    let app = d.root::<ContainersApp>();
    let chosen = app
        .selected()
        .and_then(|id| app.state().engine.find(&id).cloned())
        .map(|container| container.name)
        .expect("the staged window shows a container");
    assert_eq!(
        bar.location
            .as_ref()
            .map(|location| location.label.as_str()),
        Some(chosen.as_str()),
        "the chosen container names the window"
    );
    let names: Vec<&str> = bar
        .actions
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    assert!(
        names.contains(&"Remove"),
        "the container's verbs are bar actions: {names:?}"
    );
    assert!(
        !d.targets()
            .keys()
            .any(|id| id.starts_with("detail-remove-")),
        "the detail draws no verbs of its own"
    );
    assert!(
        !bar.actions
            .iter()
            .any(|action| action.name.contains("Search")),
        "the bar's own search is the only one: {:?}",
        bar.actions
    );
    let close = bar
        .actions
        .iter()
        .find(|action| action.name == "Close details")
        .expect("closing the detail is a bar action");
    assert_eq!(
        close.icon, "panel-right",
        "closing the detail shows the pane it closes, not a cross beside the search"
    );
    let close = close.id.clone();
    assert!(
        !d.targets().contains_key("detail-close"),
        "the window draws no header row of its own"
    );
    d.press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: close });
    // The shell presses the header's button in the frame's settle pass;
    // the window then dispatches what the button invoked.
    d.idle();
    d.relayout();
    assert!(
        app_of(&mut d).selected().is_none(),
        "the bar closed the detail"
    );
    let bar = ui::window_toolbar::declared().expect("the window's slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Containers"),
        "with nothing chosen the section names the window"
    );

    d.key("2");
    d.relayout();
    let bar = ui::window_toolbar::declared().expect("the window's slots");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Images")
    );
}

#[test]
fn escape_closes_the_detail_in_a_narrow_window_too() {
    let mut d = ui::headless::FrameHarness::for_root(
        ui::State::new(),
        staged(),
        (390, 844),
        ui::Theme::dark(),
    );
    assert!(app_of(&mut d).selected().is_some());
    assert_eq!(app_of(&mut d).state().parts.pane.get(), NavPane::Content);
    d.key("Escape");
    assert!(
        app_of(&mut d).selected().is_none(),
        "Escape clears the selection as it does in a wide window"
    );
    assert_eq!(
        app_of(&mut d).state().parts.pane.get(),
        NavPane::Overview,
        "and the list is in front again"
    );
}

#[test]
fn log_lines_read_their_severity() {
    use ui::widget::Tone;
    assert_eq!(
        view::line_severity("2026-06-30T13:05:01Z WARN  api::auth: 1 failed login"),
        Tone::Warning
    );
    assert_eq!(
        view::line_severity("WARNING: Console endpoint is listening on a dynamic port"),
        Tone::Warning
    );
    assert_eq!(
        view::line_severity("logger=sqlstore level=error msg=\"database locked\""),
        Tone::Danger
    );
    assert_eq!(
        view::line_severity("2026/06/30 09:14:01 [error] 1#1: open() failed"),
        Tone::Danger
    );
    assert_eq!(
        view::line_severity("api::http: GET /api/errors 200 12ms"),
        Tone::Neutral,
        "a word that only contains a level is not one"
    );
}

#[test]
fn removing_asks_first_and_then_runs_once() {
    let mut d = driver(staged());
    let first = view::listed_containers(&app_of(&mut d).state().engine)[0].clone();
    assert!(first.status.is_running());
    d.click_named(&format!("detail-remove-{}", first.id))
        .unwrap();
    assert_eq!(
        app_of(&mut d)
            .state()
            .confirm
            .as_ref()
            .map(|c| c.name.clone()),
        Some(first.name.clone()),
        "Remove opens the confirmation"
    );
    assert!(app_of(&mut d).state().engine.find(&first.id).is_some());
    // The dialog opens on its safe answer: Enter keeps the container.
    d.key("Enter");
    assert!(app_of(&mut d).state().confirm.is_none());
    assert!(
        app_of(&mut d).state().engine.find(&first.id).is_some(),
        "Enter on the opened confirmation cancels"
    );

    d.click_named(&format!("detail-remove-{}", first.id))
        .unwrap();
    let remove = d
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some("Remove container") && node.role() == ui::accesskit::Role::Button
        })
        .map(|(id, _)| id)
        .expect("the confirmation's Remove container action");
    d.perform_a11y_action(remove, &ui::a11y::Action::Activate)
        .unwrap();
    assert!(app_of(&mut d).state().confirm.is_none());
    assert!(
        app_of(&mut d).state().engine.find(&first.id).is_none(),
        "the confirmed removal ran"
    );
    assert!(
        app_of(&mut d).selected().is_none(),
        "the removed container is no longer selected"
    );

    let stop = view::listed_containers(&app_of(&mut d).state().engine)[0].clone();
    app_of(&mut d)
        .intents
        .push(Intent::Select(Some(stop.id.clone())));
    d.relayout();
    d.click_named(&format!("detail-stop-{}", stop.id)).unwrap();
    assert!(
        app_of(&mut d).state().confirm.is_none(),
        "stopping is not confirmed"
    );
    assert_eq!(
        app_of(&mut d)
            .state()
            .engine
            .find(&stop.id)
            .map(|c| c.status),
        Some(crate::model::ContainerStatus::Exited)
    );
}

/// Moving around is the window's own: switching sections leaves the
/// selection behind, and a selection opens the containers section.
#[test]
fn where_the_window_is_stays_local() {
    let app = staged();
    let id = app.selected().unwrap();
    app.state().show_section(Section::Images);
    assert_eq!(app.section(), Section::Images);
    assert!(app.selected().is_none());
    app.state().select_container(Some(id.clone()));
    assert_eq!(app.section(), Section::Containers);
    assert_eq!(app.selected(), Some(id));
    app.state().show_tab(DetailTab::Inspect);
    assert_eq!(app.tab(), DetailTab::Inspect);
}

#[test]
fn lifecycle_effects_and_resources_are_exported_and_bounded() {
    // --- phase: lifecycle_control_emits_the_exported_effect_instead_of_running_podman_inline ---
    {
        let window = ContainersApp::sample();
        let mut app = window.state();
        app.engine.live = true;
        let id = app.engine.containers[0].id.clone();
        app.apply_action(Action::Stop, &id);
        assert_eq!(
            app.requested.last(),
            Some(&(
                format!("container:{id}"),
                ContainersEffect::Stop {
                    container: id.clone()
                }
            )),
            "the host is asked for the exported effect on the container's row"
        );
        assert_eq!(
            app.engine.find(&id).map(|container| container.status),
            Some(crate::model::ContainerStatus::Running),
            "the control does not mutate the engine before host authorization"
        );
    }

    // --- phase: resource_snapshot_is_bounded_and_omitted_identity_resolves_lazily ---
    {
        let limit = RESOURCE_SNAPSHOT_LIMIT;
        let window = ContainersApp::sample();
        let mut app = window.state();
        let template = app.engine.containers[0].clone();
        app.engine.containers.clear();
        for index in 0..(limit + 17) {
            let mut container = template.clone();
            container.id = format!("durable-{index:03}");
            container.name = format!("Container {index:03}");
            app.engine.containers.push(container);
        }
        app.engine_changed();

        let rows = app.engine_rows();
        assert_eq!(rows.len(), limit);
        let omitted = format!("container:durable-{:03}", limit + 16);
        assert!(rows.iter().all(|row| row.id() != omitted));
        assert_eq!(
            app.engine_row(&omitted).map(|row| row.id().to_owned()),
            Some(omitted.clone())
        );

        app.engine
            .containers
            .retain(|container| format!("container:{}", container.id) != omitted);
        app.engine_changed();
        assert!(
            app.engine_row(&omitted).is_none(),
            "a stale search address cannot recreate removed authority"
        );
        assert!(
            crate::search::engine_entries(&app.engine)
                .iter()
                .all(|entry| {
                    entry
                        .action
                        .as_ref()
                        .is_none_or(|action| action.resource != omitted)
                }),
            "removed containers are absent from canonical search entries"
        );
    }
}

#[test]
fn controlled_pull_retains_channel_and_settles_once() {
    let (window, requests, _results) = ContainersApp::controlled_runtime_root();
    let mut app = window.state();
    app.start_pull(" quay.io/example/image:1 ");
    let ControlledRuntimeRequest::Pull(request) = requests.recv().unwrap() else {
        panic!("expected one controlled pull request");
    };
    assert_eq!(request.reference, "quay.io/example/image:1");
    assert_eq!(
        request.arguments,
        crate::podman::pull_args("quay.io/example/image:1")
    );
    assert!(
        requests.try_recv().is_err(),
        "pull was dispatched only once"
    );

    request.line("Copying blob 1/1").unwrap();
    request.done().unwrap();
    app.drain_bg();
    let pull = app.pull.as_ref().expect("retained pull progress");
    assert_eq!(
        pull.lines.last().map(String::as_str),
        Some("Copying blob 1/1")
    );
    assert!(pull.done);
    assert!(pull.failed.is_none());
    assert!(matches!(
        requests.recv().unwrap(),
        ControlledRuntimeRequest::Refresh(_)
    ));
    let terminal = app.toast.deadline();
    app.drain_bg();
    assert_eq!(app.toast.deadline(), terminal);
    assert!(
        requests.try_recv().is_err(),
        "terminal event was reduced once"
    );
}

#[test]
fn controlled_runtime_owns_refresh_cadence() {
    let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
    app.state().last_refresh = Instant::now()
        .checked_sub(crate::podman::REFRESH_INTERVAL + std::time::Duration::from_millis(1))
        .expect("refresh interval fits below Instant::now");
    let doc = AutoCommit::new();
    app.sync(&doc, &automerge::ROOT);
    assert!(
        requests.try_recv().is_err(),
        "controlled fixtures own refresh; wall-clock must not enqueue Refresh"
    );
}

#[test]
fn controlled_create_runs_the_draft_and_drains_one_snapshot() {
    let (window, requests, _results) = ContainersApp::controlled_runtime_root();
    let mut app = window.state();
    let mut record = ui::tilo_resource::Replica::<Containers>::new();
    for (field, text) in [
        (CreateField::Name, "web"),
        (CreateField::Image, "nginx:1"),
        (CreateField::Ports, "8080:80"),
    ] {
        record
            .apply(ui::tilo_resource::Invoke::edit(&field.path().replace(text)).unwrap())
            .unwrap();
    }
    let mut doc = record.doc().clone();
    app.set_create_open(true);
    app.submit_create(&mut ui::widget::Edits::lend(&mut doc));
    let ControlledRuntimeRequest::Create(request) = requests.recv().unwrap() else {
        panic!("expected one controlled create request");
    };
    assert_eq!(
        request.arguments,
        vec!["run", "-d", "--name", "web", "-p", "8080:80", "nginx:1"]
    );
    assert!(!app.create_open);

    let refreshed = Engine {
        live: true,
        connection: "controlled post-run snapshot".into(),
        ..Engine::default()
    };
    request.post_run_snapshot(refreshed).unwrap();
    app.drain_bg();
    assert_eq!(app.engine.connection, "controlled post-run snapshot");
    app.drain_bg();
    assert!(
        requests.try_recv().is_err(),
        "create was dispatched only once"
    );
}

#[test]
fn controlled_immediate_edges_capture_exact_requests() {
    let (window, requests, results) = ContainersApp::controlled_runtime_root();
    let mut app = window.state();

    results.send(Ok(())).unwrap();
    app.tag_image("quay.io/example/source:1", " registry.local/copy:2 ");
    match requests.recv().unwrap() {
        ControlledRuntimeRequest::Immediate {
            operation,
            target,
            arguments,
        } => {
            assert_eq!(operation, "containers.image.tag");
            assert_eq!(target, "quay.io/example/source:1");
            assert_eq!(
                arguments,
                ["tag", "quay.io/example/source:1", "registry.local/copy:2"]
            );
        }
        _ => panic!("expected captured tag request"),
    }
    assert!(matches!(
        requests.recv().unwrap(),
        ControlledRuntimeRequest::Refresh(_)
    ));

    results.send(Ok(())).unwrap();
    app.create_volume(" cache ");
    match requests.recv().unwrap() {
        ControlledRuntimeRequest::Immediate {
            operation,
            target,
            arguments,
        } => {
            assert_eq!(operation, "containers.volume.create");
            assert_eq!(target, "cache");
            assert_eq!(arguments, ["volume", "create", "cache"]);
        }
        _ => panic!("expected captured volume request"),
    }
    assert!(matches!(
        requests.recv().unwrap(),
        ControlledRuntimeRequest::Refresh(_)
    ));
    assert!(requests.try_recv().is_err());
}

#[test]
fn a_lifecycle_control_asks_the_host_for_its_exact_target() {
    let (mut app, requests, _results) = ContainersApp::controlled_runtime_root();
    app.state().apply_action(Action::Restart, "retained-target");
    let doc = AutoCommit::new();
    app.sync(&doc, &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(
        app.state().requested,
        [(
            "container:retained-target".to_owned(),
            ContainersEffect::Restart {
                container: "retained-target".into()
            }
        )],
        "one effect on exactly that container's row"
    );
    assert!(
        requests.try_recv().is_err(),
        "nothing reaches the engine before the host answers"
    );
}

/// A container that logs after it was opened says so: the detail read
/// beside the probe replaces what the tab showed, instead of the first
/// read being carried over for as long as the container stays open
/// (`APP-containers-01`).
#[test]
fn a_container_that_logs_later_replaces_the_lines_the_tab_shows() {
    let (window, _requests, _results) = ContainersApp::controlled_runtime_root();
    let mut app = window.state();
    app.show_engine(ContainersApp::controlled_engine("controlled Podman", true));
    app.select_container(Some("armed-container".into()));
    let mut fresh = app
        .engine
        .find("armed-container")
        .cloned()
        .expect("the container is there");
    fresh.logs = vec!["error: upstream timed out".into()];
    fresh.logs_fetched_unix_ms = Some(1_700_000_000_000);
    app.bg_tx
        .as_ref()
        .expect("the controlled runtime has a channel")
        .send(Bg::Detail(Box::new(fresh)))
        .unwrap();
    app.drain_bg();
    let shown = app.engine.find("armed-container").expect("still there");
    assert_eq!(shown.logs, ["error: upstream timed out".to_string()]);
    assert_eq!(shown.logs_fetched_unix_ms, Some(1_700_000_000_000));
}

/// Stopping a pull asks the engine to end it, and the sheet says it
/// stopped rather than pretending it finished.
#[test]
fn stopping_a_pull_ends_it_rather_than_hiding_it() {
    let (window, _requests, _results) = ContainersApp::controlled_runtime_root();
    let mut app = window.state();
    app.start_pull("docker.io/library/alpine:3.20");
    let pull = app.pull.as_ref().expect("the pull sheet is open");
    assert!(!pull.stop.load(std::sync::atomic::Ordering::Acquire));
    app.stop_pull();
    let pull = app.pull.as_ref().expect("the sheet stays open");
    assert!(pull.stop.load(std::sync::atomic::Ordering::Acquire));
    assert!(!pull.stopped, "the engine has not confirmed yet");
    app.apply_pull_msg(StreamMsg::Stopped);
    assert!(app.pull.as_ref().is_some_and(|p| p.stopped));
}

/// What Podman's layer lines say about the bytes fetched.
#[test]
fn a_layer_line_says_how_many_bytes_are_fetched() {
    assert_eq!(
        pulled_bytes("Copying blob sha256:9fa9226be034 2.1 MiB / 3.4 MiB"),
        Some((2_202_009, Some(3_565_158)))
    );
    assert_eq!(pulled_bytes("Copying blob sha256:9fa9226be034 done"), None);
    assert_eq!(
        pulled_bytes("Trying to pull docker.io/library/alpine:3.20"),
        None
    );
}

/// A controlled live engine selects and hydrates its one container.
#[test]
fn controlled_selection_asks_for_its_detail() {
    let (window, requests, _results) = ContainersApp::controlled_runtime_root();
    let mut app = window.state();
    app.show_engine(ContainersApp::controlled_engine("controlled Podman", false));
    app.select_container(Some("armed-container".into()));
    app.hydrate_selection();
    assert!(matches!(
        requests.recv().unwrap(),
        ControlledRuntimeRequest::SelectedDetails { target } if target == "armed-container"
    ));
    app.show_engine(ContainersApp::controlled_engine("controlled Podman", true));
    app.hydrate_selection();
    assert!(
        requests.try_recv().is_err(),
        "a hydrated container is not asked again"
    );
}
