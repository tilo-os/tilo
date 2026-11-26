use super::*;
use crate::testing::dispatch;
use automerge::AutoCommit;
use vello::Scene;

fn lay_out(w: &mut dyn Widget, vw: f32, vh: f32) {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let node = w.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(vw),
                height: taffy::AvailableSpace::Definite(vh),
            },
            |k, a, id, _, _| {
                node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                    (*p).measure(k, a, &mut tr)
                })
            },
        )
        .unwrap();
    w.apply_layout(&taffy, node, Point::ZERO);
}

fn draw(w: &mut dyn Widget) -> Scene {
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    w.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        presence.draw_env(),
    ));
    scene
}

fn cursor(p: Point) -> WindowEvent {
    tilo_ui_base::synthetic_input::cursor_moved(p.x, p.y)
}
use tilo_ui_base::synthetic_input::mouse_press;

tilo_test_env::scenario! {
    /// What an empty state shows in the width it has.
    /// Each phase was a test of its own and builds its own fixture.
    fn an_empty_state_lays_out_wraps_and_sheds;






    phase an_inline_empty_state_hugs_its_content_and_a_busy_one_keeps_moving;
    {
        let mut inline = EmptyState::new("No servers yet")
            .with_message("Add one to start.")
            .inline();
        let mut full = EmptyState::new("No servers yet").with_message("Add one to start.");
        lay_out(&mut inline, 600.0, 600.0);
        lay_out(&mut full, 600.0, 600.0);
        assert!(
            inline.get_rect().height() < 120.0,
            "inline hugs its content: {:?}",
            inline.get_rect()
        );
        assert!(full.get_rect().height() >= 220.0);

        // The glyph it was given is what tells a person what would be here: a state that
        // hugs its content keeps it rather than shedding it.
        let mut with_icon = EmptyState::new("No archived items")
            .with_message("Archive an item to put it aside.")
            .with_icon(tilo_ui_base::icons::Icon::Archive)
            .inline();
        lay_out(&mut with_icon, 600.0, 600.0);
        let rect = with_icon.get_rect();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let placed = place(&mut tr, &theme, rect, &with_icon.content(), true);
        assert!(placed.hero.is_some(), "an inline state shows its glyph in {rect:?}, natural {}", placed.natural);

        let busy = EmptyState::new("Scanning your disk").busy();
        assert!(busy.needs_animation_frame(), "a busy state keeps moving");
        let node = Widget::accessibility(&busy).unwrap();
        assert!(node.state.busy);
    }
}

tilo_test_env::scenario! {
    /// An empty state's action.
    /// Each phase was a test of its own and builds its own fixture.
    fn the_empty_state_action_is_placed_published_and_fires_once;


    phase empty_state_action_fires_once_through_pointer_keyboard_and_semantics;
    {
        fn action_pill_click_fires_once() {
            let fired = Rc::new(std::cell::Cell::new(0));
            let counted = Rc::clone(&fired);
            let mut es = EmptyState::new("No messages")
                .with_icon(Icon::Inbox)
                .with_message("Your inbox is empty")
                .with_action("Compose")
                .on_open(move || counted.set(counted.get() + 1));
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            let target = es.action_rect().center();
            assert!(target.x > 0.0, "action rect must resolve after draw");
            dispatch(&mut es, &cursor(target));
            assert!(
                dispatch(&mut es, &mouse_press()),
                "click on the action pill is consumed"
            );
            assert_eq!(fired.get(), 1, "the action runs once per click");
        }

        fn click_outside_action_does_not_fire() {
            let mut es = EmptyState::new("Nothing here")
                .with_action("Add")
                .on_open(|| panic!("a click away from the pill runs nothing"));
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            dispatch(&mut es, &cursor(Point::new(2.0, 2.0)));
            assert!(
                !dispatch(&mut es, &mouse_press()),
                "click away from the pill is not consumed"
            );
        }

        fn action_is_keyboard_reachable_and_exposed_as_a_named_button() {
            let fired = Rc::new(std::cell::Cell::new(false));
            let seen = Rc::clone(&fired);
            let mut es = EmptyState::new("No notes yet")
                .with_message("Create one to get started")
                .with_action("Create note")
                .on_open(move || seen.set(true));
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            assert!(Widget::is_focusable(&es));
            Widget::set_focus(&mut es, true);
            let node = Widget::accessibility(&es).expect("empty state is semantic");
            assert!(node.state.focusable && node.state.focused);
            assert_eq!(node.children.len(), 1);
            assert_eq!(node.children[0].role, tilo_ui_base::a11y::Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("Create note"));
            assert!(node.children[0].state.focused);
            assert_eq!(
                es.synthetic_accessibility_rect(&[0]),
                Some(es.action_rect())
            );

            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                &mut es,
                &tilo_ui_base::a11y::Action::Activate,
                &[],
                &mut tilo_ui_base::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(fired.get(), "Enter/Space/AT Activate reaches the CTA");
        }

        fn action_path_is_shared_by_pointer_and_accessibility_activation() {
            use automerge::ReadDoc as _;
            use std::cell::Cell;
            let invoked = Rc::new(Cell::new(0));
            let counted = invoked.clone();
            let empty = EmptyState::new("Nothing here")
                .with_action("Open")
                .with_action_id("empty.open")
                .on_open(move || counted.set(counted.get() + 1));
            let mut harness = crate::app::driver::Driver::for_root(
                crate::State::new(),
                empty,
                (600, 400),
                Theme::dark(),
            );
            let action = harness.root::<EmptyState>().action_rect();
            assert!(action.area() > 0.0, "the recovery action has a target");
            harness.click_at(action.center().x, action.center().y);
            assert_eq!(
                invoked.get(),
                1,
                "pointer activation invokes the published action"
            );
            harness
                .perform_named("empty.open", &tilo_ui_base::a11y::Action::Activate)
                .unwrap();
            assert_eq!(
                invoked.get(),
                2,
                "accessibility invokes the same published action"
            );
            assert!(
                harness
                    .state()
                    .doc
                    .get(automerge::ROOT, "empty.open")
                    .unwrap()
                    .is_none(),
                "a view action never writes a click pulse into the resource"
            );
        }

        fn an_action_binding_invokes_the_resource_action() {
            let resource = tilo_actions::ResourceSpec::new("catalog", "catalog", "Catalog")
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "refresh",
                        "Refresh catalog",
                        tilo_actions::OperationKind::Effect,
                        "test.refresh",
                        &(),
                    )
                    .unwrap(),
                );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "refresh").unwrap();
            let mut es = EmptyState::new("Unavailable")
                .with_action("Refresh catalog")
                .with_action_id("catalog.empty.refresh")
                .on_action(binding);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            let target = es.action_rect().center();
            dispatch(&mut es, &cursor(target));
            assert!(dispatch(&mut es, &mouse_press()));
            let invocation = tilo_ui_base::widget::controls::take_control_invocation(&mut es)
                .expect("canonical invocation");
            assert_eq!(
                invocation.address,
                tilo_actions::ActionAddress::new("catalog", "refresh")
            );
            assert!(matches!(
                tilo_ui_base::widget::controls::control_action_bindings(&es).as_slice(),
                [tilo_ui_base::widget::ControlBindingMetadata {
                    control_id,
                    kind: tilo_ui_base::widget::ControlBindingKind::Action(address),
                }] if control_id == "catalog.empty.refresh"
                    && address == &tilo_actions::ActionAddress::new("catalog", "refresh")
            ));
        }

        action_pill_click_fires_once();
        click_outside_action_does_not_fire();
        action_is_keyboard_reachable_and_exposed_as_a_named_button();
        action_path_is_shared_by_pointer_and_accessibility_activation();
        an_action_binding_invokes_the_resource_action();
    }

    phase a_handler_publishes_the_action_as_a_certified_bound_button;
    {
        let opened = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = opened.clone();
        let root = EmptyState::new("No places yet")
            .with_action("Add a place")
            .with_action_id("weather.empty.add")
            .on_open(move || seen.set(seen.get() + 1));
        let mut driver =
            crate::app::driver::Driver::new(crate::state::State::new(), Box::new(root), (480, 360));
        let audit = driver.native_app_audit();
        assert!(audit.is_certified(), "{audit:#?}");
        driver
            .perform_named("weather.empty.add", &tilo_ui_base::a11y::Action::Activate)
            .expect("the action is addressable by its id");
        assert_eq!(opened.get(), 1, "the view handler ran once");
    }


    phase a_painted_state_is_addressed_as_the_controls_beside_it_are;
    {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        let content = EmptyStateContent {
            action_label: Some("Choose camera"),
            ..EmptyStateContent::new("Camera unavailable")
        };
        let mut painted = PaintedEmptyStates::default();
        painted.draw(Affine::IDENTITY, || {
            EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                Rect::new(0.0, 0.0, 500.0, 400.0),
                &content,
                Point::new(-1.0, -1.0),
            );
        });
        let host = EmptyState::new("host");
        let control = || AccessNode::new(Role::Button).with_name("Take photo");

        let keyed = AccessNode::new(Role::Window)
            .with_children(vec![control().with_action_target("camera.shutter")]);
        let keyed = painted.describe(Some(keyed), &host).unwrap();
        assert_eq!(
            keyed.children.len(),
            2,
            "the painted state joins the controls"
        );
        assert!(
            keyed.children.iter().all(|child| child.key.is_some()),
            "beside keyed controls it is keyed too"
        );

        let positional = AccessNode::new(Role::Window).with_children(vec![control()]);
        let positional = painted.describe(Some(positional), &host).unwrap();
        assert_eq!(positional.children.len(), 2);
        assert!(
            positional.children.iter().all(|child| child.key.is_none()),
            "beside position-addressed controls it is addressed by position"
        );
    }

}

tilo_test_env::scenario! {
    /// An empty state's details disclosure.
    /// Each phase was a test of its own and builds its own fixture.
    fn technical_details_wait_behind_their_disclosure;

    phase the_details_disclosure_is_an_inventoried_view_control;
    {
        let es = EmptyState::new("Unavailable")
            .with_details("os error 2")
            .on_click(|| {})
            .with_action("Try again");
        let target = es
            .accessibility()
            .unwrap()
            .children
            .iter()
            .find(|child| child.description.as_deref() == Some("os error 2"))
            .and_then(|child| child.action_target.clone())
            .expect("the disclosure names its control");
        let bindings = tilo_ui_base::widget::controls::control_action_bindings(&es);
        assert!(
            bindings.iter().any(|binding| binding.control_id == target
                && matches!(
                    binding.kind,
                    tilo_ui_base::widget::ControlBindingKind::SemanticAction(_)
                )),
            "{bindings:?}"
        );
        assert!(
            tilo_ui_base::widget::controls::control_action_bindings(
                &EmptyState::new("Empty").with_details("why")
            )
            .iter()
            .any(|binding| binding.control_id.ends_with(":details"))
        );
    }

    phase technical_details_wait_behind_a_closed_disclosure_under_the_action;
    {
        let detail = "command process failed: No such file or directory (os error 2)";
        let mut es = EmptyState::new("Podman isn't available")
            .with_message("Install Podman to manage containers here.")
            .with_action("Try again")
            .on_open(|| panic!("opening details is not the action"))
            .with_details(detail);
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);
        let toggle = es.details_rect;
        assert!(!es.details_open(), "details start closed");
        assert!(
            toggle.y0 >= es.action_rect().y1,
            "the disclosure sits under the action: {toggle:?} {:?}",
            es.action_rect()
        );
        let node = Widget::accessibility(&es).unwrap();
        assert!(
            !node
                .description
                .clone()
                .unwrap_or_default()
                .contains("os error"),
            "the message stays the person's"
        );
        let disclosure = node
            .children
            .iter()
            .find(|child| child.description.as_deref() == Some(detail))
            .expect("the detail is spoken from the disclosure");
        assert_eq!(disclosure.state.expanded, Some(false));

        dispatch(&mut es, &cursor(toggle.center()));
        assert!(dispatch(&mut es, &mouse_press()), "the click opens it");
        assert!(es.details_open());
        let node = Widget::accessibility(&es).unwrap();
        assert!(
            node.children
                .iter()
                .any(|child| child.state.expanded == Some(true))
        );
    }

}

#[test]
fn an_empty_state_in_a_workspace_that_never_synced_says_so_and_offers_nothing_of_the_apps() {
    use tilo_ui_base::{workspace_sync::WorkspaceSync, workspace_sync::WorkspaceSyncState};
    let theme = tilo_ui_base::theme::Theme::dark();
    let mut text = tilo_ui_base::text_render::TextRenderer::new();
    let mut scene = vello::Scene::new();
    let rect = vello::kurbo::Rect::new(0.0, 0.0, 500.0, 400.0);
    let content = EmptyStateContent {
        action_label: Some("Say hello"),
        ..EmptyStateContent::new("Nothing has been said here yet")
    };
    let mut paint = |scene: &mut vello::Scene| {
        EmptyState::paint(
            &mut PaintCtx::new(scene, vello::kurbo::Affine::IDENTITY, &mut text, &theme),
            rect,
            &content,
            vello::kurbo::Point::new(-1.0, -1.0),
        )
    };
    let told = |state| {
        Some(WorkspaceSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
            unsynced_writes: 0,
            cannot_keep_writes: false,
        })
    };

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::Connecting));
    let _ = tilo_ui_base::workspace_sync::take_spinner_damage();
    assert!(
        paint(&mut scene).is_none(),
        "before the first sync the app's own empty state, and its action, wait"
    );
    assert!(
        tilo_ui_base::workspace_sync::take_spinner_damage().is_some_and(|slot| {
            rect.contains(slot.center()) && slot.area() < rect.area() / 4.0
        }),
        "the connecting state turns a spinner and damages its slot alone"
    );

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::Unreachable));
    assert!(
        paint(&mut scene).is_some(),
        "with its host out of reach the person works here: the app's action stays"
    );
    assert!(
        tilo_ui_base::workspace_sync::take_spinner_damage().is_none(),
        "a host out of reach is no wait to spin through"
    );

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::HostNeedsUpdate));
    assert!(paint(&mut scene).is_none());
    assert!(
        tilo_ui_base::workspace_sync::take_spinner_damage().is_none(),
        "a host that has to update is no wait to spin through"
    );

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::Synced));
    assert!(
        paint(&mut scene).is_some(),
        "once the workspace synced, its emptiness is the app's to say"
    );
    tilo_ui_base::workspace_sync::set(None);
    assert!(
        paint(&mut scene).is_some(),
        "a workspace with no sync state is the app's"
    );
}

#[test]
fn a_screen_reader_hears_what_the_screen_says_before_the_workspace_synced() {
    use tilo_ui_base::{workspace_sync::WorkspaceSync, workspace_sync::WorkspaceSyncState};
    let empty = EmptyState::new("No notes yet").with_action("New note");
    let told = |state| {
        Some(WorkspaceSync {
            state,
            workspace: "Lounge".into(),
            writable: None,
            unsynced_writes: 0,
            cannot_keep_writes: false,
        })
    };

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::Connecting));
    let waiting = Widget::accessibility(&empty).unwrap();
    assert_eq!(
        waiting.name.as_deref(),
        Some(
            tilo_ui_base::workspace_sync::waiting()
                .unwrap()
                .title
                .as_str()
        )
    );
    assert!(waiting.name.as_deref().unwrap().contains("Lounge"));
    assert!(waiting.state.busy, "the connecting state is on its way");
    assert!(waiting.children.is_empty(), "the app's action waits too");

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::Unreachable));
    let unreachable = Widget::accessibility(&empty).unwrap();
    assert!(unreachable.name.as_deref().unwrap().contains("Lounge"));
    assert!(!unreachable.state.busy, "nothing is on its way");
    assert!(
        unreachable.description.is_some(),
        "it says changes stay here"
    );
    assert_eq!(
        unreachable
            .children
            .first()
            .and_then(|action| action.name.as_deref()),
        Some("New note"),
        "the person works on what is here"
    );

    tilo_ui_base::workspace_sync::set(told(WorkspaceSyncState::Synced));
    let empty_node = Widget::accessibility(&empty).unwrap();
    assert_eq!(empty_node.name.as_deref(), Some("No notes yet"));
    assert!(!empty_node.state.busy);
    assert_eq!(
        empty_node
            .children
            .first()
            .and_then(|action| action.name.as_deref()),
        Some("New note")
    );
    tilo_ui_base::workspace_sync::set(None);
}

/// Open Details read in the order they were said, one line each, from one left edge; a command to
/// run stands on a line of its own, and its Copy action, pressed or called, copies exactly it.
#[test]
fn details_keep_their_lines_and_copy_the_command_they_name() {
    use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
    let clipboard = std::sync::Arc::new(InProcessClipboard::default());
    let services = crate::process::ProcessServices::null().with_clipboard(clipboard.clone());
    crate::process::scoped(&services, || {
        let mut es = EmptyState::new("Settings could not open")
            .with_action("Try again")
            .on_open(|| {})
            .with_details(
                "kernel.x turns off user namespaces, which every app's sandbox is made in.\n\
                 Turn them on:\n    sudo sysctl kernel.x=1\n\
                 Keep them on after a restart with this line in a file under /etc/sysctl.d/:\n    \
                 kernel.x = 1\n\
                 so.tilo.settings ended: exit status: 1",
            );
        lay_out(&mut es, 600.0, 520.0);
        es.toggle_details();
        lay_out(&mut es, 600.0, 520.0);
        let lines = es.details_lines.clone();
        let said: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            said,
            [
                "kernel.x turns off user namespaces, which every app's sandbox is made in.",
                "Turn them on:",
                "sudo sysctl kernel.x=1",
                "Keep them on after a restart with this line in a file under /etc/sysctl.d/:",
                "kernel.x = 1",
                "so.tilo.settings ended: exit status: 1",
            ]
        );
        assert!(
            lines
                .windows(2)
                .all(|pair| pair[0].rect.y1 <= pair[1].rect.y0
                    && (pair[0].rect.x0 - pair[1].rect.x0).abs() < f64::EPSILON),
            "one line under the other, from one left edge: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| es.get_rect().contains(line.rect.center())),
            "inside the state: {lines:?}"
        );
        let command = lines
            .iter()
            .position(|line| line.text == "sudo sysctl kernel.x=1")
            .unwrap();
        assert_eq!(lines[command].kind, details::Kind::Verbatim);
        assert!(lines.iter().filter(|line| line.copy.is_some()).count() == 2);

        let copy = lines[command].copy.unwrap();
        dispatch(&mut es, &cursor(copy.center()));
        assert!(dispatch(&mut es, &mouse_press()), "the press copies");
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("sudo sysctl kernel.x=1")
        );
        assert!(es.details_open(), "copying leaves the Details open");

        let node = Widget::accessibility(&es).unwrap();
        let file_line = node
            .children
            .iter()
            .find(|child| child.value.as_deref() == Some("kernel.x = 1"))
            .expect("the line for the file is read exactly");
        assert!(file_line.state.read_only);
        let target = file_line.children[0].action_target.clone().unwrap();
        assert!(
            tilo_ui_base::widget::controls::control_action_bindings(&es)
                .iter()
                .any(|binding| binding.control_id == target),
            "copying it is a control agents find"
        );
        assert!(Widget::perform_semantic_action(
            &mut es,
            &target,
            &tilo_ui_base::a11y::Action::Activate,
            &mut tilo_ui_base::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            clipboard.text(ClipboardTarget::Clipboard).as_deref(),
            Some("kernel.x = 1")
        );
    });
}

/// A failure says all of what happened, to a reader too, and its glyph wears the danger colour.
#[test]
fn errors_use_the_danger_tone_and_wrap_their_whole_message_at_420_px() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let detail = "Couldn't read the selected files. Check permissions and try again.";
    let content = EmptyStateContent {
        icon: Some(Icon::FileText),
        message: Some(detail),
        error: true,
        ..EmptyStateContent::new("Couldn't open the files")
    };
    let style = StackStyle::resolved(&theme, false);
    let error = style.measure(&mut text, 420.0, &content, 0.0);
    let plain = style.measure(
        &mut text,
        420.0,
        &EmptyStateContent {
            error: false,
            ..content
        },
        0.0,
    );
    let (_, line) = text.measure_style("Ag", None, &style.message);
    assert!(
        error.msg_h > f64::from(line) * 1.5,
        "the whole message wraps onto more lines than one"
    );
    assert!(
        (error.msg_h - plain.msg_h).abs() < f64::EPSILON,
        "an error wraps exactly like any other message"
    );
    assert_eq!(error.text_gap, f64::from(theme.tokens.spacing.s3));
    assert_eq!(error.gap, f64::from(theme.tokens.spacing.s4));

    let state = EmptyState::error("Couldn't open the files", detail);
    let node = Widget::accessibility(&state).unwrap();
    assert_eq!(node.description.as_deref(), Some(detail), "full text");

    let mut scene = Scene::new();
    paint_empty_state(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        Rect::new(0.0, 0.0, 360.0, 400.0),
        &content,
        Point::new(-1.0, -1.0),
        false,
        false,
        None,
    );
    let danger = theme.tokens.colors.semantic.danger.to_rgba8();
    let encoded = scene.encoding();
    let has_danger = encoded.draw_data.chunks(1).any(|word| {
        word.first().is_some_and(|w| {
            let [r, g, b, a] = w.to_le_bytes();
            (r, g, b, a) == (danger.r, danger.g, danger.b, danger.a)
                || (a, b, g, r) == (danger.r, danger.g, danger.b, danger.a)
        })
    });
    assert!(has_danger, "error glyph is painted in the danger colour");
}
