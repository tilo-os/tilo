//! A retained native shell frame, admitted paint, and the real semantic editor handoff.
use super::CertifiedCursorFixture;
use crate::shell::{frame::FrameBuilder, overlay_host::AccessibleAgentMarker};
use base64::Engine as _;
use std::time::{Duration, Instant};
use ui::widget::Widget;
use vello::kurbo::{Affine, Rect};

const WIDTH: u32 = 1600;
const HEIGHT: u32 = 900;

fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now().checked_add(Duration::from_secs(6)).unwrap();
    while !ready() {
        assert!(Instant::now() < deadline, "{description}");
        std::thread::yield_now();
    }
}

fn editor(widget: &mut dyn Widget) -> Option<&mut ui::widget::TextInput> {
    if widget.test_id() == Some("agent-quick-editor") {
        return widget.as_any_mut()?.downcast_mut();
    }
    for child in widget.children_mut() {
        if let Some(editor) = editor(child) {
            return Some(editor);
        }
    }
    None
}

fn activate(frame: &mut FrameBuilder, label: &str, doc: &mut automerge::AutoCommit) {
    let tree = frame.build_a11y_tree_update(None, 1.0);
    let target = tree
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .unwrap_or_else(|| panic!("the production semantic control {label:?} is absent"))
        .0;
    assert!(frame.dispatch_a11y_action(
        target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(doc),
        &automerge::ROOT,
        &mut None
    ));
}

fn capture(frame: &mut FrameBuilder, pointer: &vello::Scene, path: &std::path::Path) -> Vec<u8> {
    frame.relayout();
    let mut retained = frame.paint_with_damage();
    assert!(!retained.scene.encoding().is_empty());
    // The native writer frame is retained independently of its shell overlay.
    // Reusing these exact commands models old pixels, not a new Agent activity.
    retained.scene.append(pointer, None);
    let mut bridge = crate::shell::render::paint::VelloDrmBridge::new(WIDTH, HEIGHT)
        .expect("the capture must observe its actual scene, not skip without an adapter");
    bridge.wait_for_gpu_completion_for_test();
    let pitch = usize::try_from(WIDTH).unwrap().checked_mul(4).unwrap();
    let mut pixels = vec![0; pitch.checked_mul(usize::try_from(HEIGHT).unwrap()).unwrap()];
    bridge
        .paint_into_xrgb8888(&retained.scene, &mut pixels, pitch)
        .unwrap();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::save_buffer(path, &pixels, WIDTH, HEIGHT, image::ColorType::Rgba8).unwrap();
    println!(
        "CERTIFIED_CAPTURE_PNG {} {}",
        path.display(),
        base64::engine::general_purpose::STANDARD.encode(std::fs::read(path).unwrap())
    );
    pixels
}

#[test]
fn certified_cursor_retained_frame_opens_original_private_task_after_same_agent_starts_b() {
    let mut fixture = CertifiedCursorFixture::new();
    let original = fixture.invoke_a();
    let room = fixture.native.room.to_string();
    let tree = crate::shell::native_workspace_owner::test_tree_of(fixture.owner());
    let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
        fixture.native.certificate.user_public_key,
    );
    let review = crate::shell::task_review::NativeTaskReview::new(tree.clone(), None).unwrap();
    wait_for("native owner must publish the approved task", || {
        review.snapshot(&room).is_some_and(|snapshot| {
            snapshot.is_ok_and(|s| s.tasks.iter().any(|t| t.id == fixture.task_a))
        })
    });
    let mut frames = Vec::new();
    for (name, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
        let theme = theme.with_reduce_motion(true);
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            ui::font_catalog::checkout_root(),
            "en-US",
        );
        let renderer = ui::text_render::TextRenderer::with_font_catalog(&catalog).unwrap();
        let inputs = crate::shell::fixture_inputs::ShellFixtureInputs::new(
            fixture.native.home.config(),
            renderer,
            theme.clone(),
        )
        .unwrap();
        let mut frame =
            FrameBuilder::with_native_fixture_inputs(tree.clone(), (WIDTH, HEIGHT), inputs)
                .unwrap();
        frame.chrome_handle().borrow_mut().act_as(person.clone());
        frame
            .host_mut()
            .attach_app_action_broker(fixture.actions.clone());
        frame.host_mut().attach_task_review(review.clone());
        frame
            .apply_native_interaction_for_test(original.clone())
            .unwrap();
        frame.relayout();
        let mut presence = ui::PresenceCtx::empty();
        presence.apply_interaction(original.clone()).unwrap();
        let mut pointer = vello::Scene::new();
        let mut text = ui::text_render::TextRenderer::new();
        let viewport = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
        let locus = Rect::new(360.0, 360.0, 520.0, 404.0);
        let geometry = presence
            .draw_agent_action_marker(
                &mut ui::paint::PaintCtx::new(&mut pointer, Affine::IDENTITY, &mut text, &theme),
                &original,
                locus,
                viewport,
                "notes:release",
                ui::time::now(),
            )
            .expect("the genuine broker-admitted action must paint a visible cursor");
        assert!(geometry.bounds.area() > 0.0 && !pointer.encoding().is_empty());
        let task = frame
            .host()
            .certified_marker_task(&room, "so.tilo.notes", "scribe", &original)
            .expect("the broker must resolve the admitted original task");
        assert_eq!(task.task_id, fixture.task_a);
        let marker = AccessibleAgentMarker {
            connection: 71,
            surface: 72,
            app_id: "so.tilo.notes".into(),
            room: room.clone(),
            agent: "scribe".into(),
            task,
            interaction: original.clone(),
            marker: geometry.bounds,
            locus,
            protected: locus.union(geometry.bounds),
            label: "Scribe".into(),
            description: presence.interaction_status_label(&original),
            published_at: Instant::now(),
        };
        frame
            .host_mut()
            .publish_accessible_agent_markers(marker.connection, vec![marker.clone()]);
        frame.relayout();
        let _ = frame.paint_with_damage();
        let label = crate::shell::i18n::t_args(
            "agent-marker-message",
            &[("task", marker.task.label.clone().into())],
        );
        let mut doc = automerge::AutoCommit::new();
        assert!(
            !frame
                .host()
                .accessible_agent_marker_request_is_inert(&marker.action_target()),
            "painted marker unexpectedly inert; kind={:?}",
            frame
                .presence()
                .participant_kind(&ui::permissions::UserId(original.actor.id.clone()))
        );
        activate(&mut frame, &label, &mut doc);
        let selected = frame
            .host()
            .take_accessible_agent_marker_activation()
            .expect("semantic activation must retain the exact painted interaction");
        assert_eq!(selected.interaction, original);
        assert_eq!(selected.task.task_id, fixture.task_a);
        let source = frame.host().marker_action_session_source().unwrap();
        wait_for("provisioned identity custody must settle", || {
            source.identity_revision(&room).is_some()
        });
        assert!(frame.host_mut().open_certified_agent_marker(selected));
        frame.pump_apps(&mut doc, &automerge::ROOT);
        frame.relayout();
        let _ = frame.paint_with_damage();
        assert_eq!(
            frame
                .host_mut()
                .agent_quick_composer_mut()
                .marker
                .conversation_task(),
            Some((fixture.task_a.clone(), 1))
        );
        let base =
            std::path::PathBuf::from("target/finish-verification/certified-cursor").join(name);
        capture(&mut frame, &pointer, &base.join("a-ready.png"));
        let input = editor(frame.host_mut()).expect("production private editor is visible");
        assert!(input.perform_action(
            &ui::a11y::Action::SetValue("Keep the original references".into()),
            &[],
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        input.set_selection(12, 5);
        input.restore_private_preedit("候補", Some((0, 3)));
        let expected = input.private_draft_editor();
        assert_eq!((expected.anchor, expected.caret), (12, 5));
        assert!(
            doc.get_heads().is_empty(),
            "private typing must not enter the room document"
        );
        let positive = capture(&mut frame, &pointer, &base.join("a-private-draft.png"));
        let without = capture(
            &mut frame,
            &vello::Scene::new(),
            &base.join("negative-no-pointer.png"),
        );
        let pointer_pixels = positive
            .chunks_exact(4)
            .zip(without.chunks_exact(4))
            .enumerate()
            .filter(|(index, (painted, absent))| {
                let index = u32::try_from(*index).unwrap();
                geometry.bounds.contains(vello::kurbo::Point::new(
                    f64::from(index % WIDTH),
                    f64::from(index / WIDTH),
                )) && painted != absent
            })
            .count();
        assert!(
            pointer_pixels > 10,
            "negative control must remove the actual visible cursor, not merely change unrelated pixels"
        );
        assert_eq!(
            editor(frame.host_mut())
                .unwrap()
                .private_draft_editor()
                .preedit,
            expected.preedit,
            "composition lost during capture"
        );
        frames.push((frame, pointer, doc, expected, base, marker));
    }
    let b = fixture.finish_a_start_b();
    assert_ne!(b, fixture.task_a);
    wait_for("native review must observe B without retargeting A", || {
        review
            .snapshot(&room)
            .is_some_and(|snapshot| snapshot.is_ok_and(|s| s.tasks.iter().any(|t| t.id == b)))
    });
    for (mut frame, pointer, mut doc, expected, base, mut marker) in frames {
        assert_eq!(
            editor(frame.host_mut())
                .unwrap()
                .private_draft_editor()
                .preedit,
            expected.preedit,
            "composition lost before reopen"
        );
        frame.host_mut().agent_quick_composer_mut().dismiss();
        marker.published_at = Instant::now();
        frame
            .host_mut()
            .publish_accessible_agent_markers(marker.connection, vec![marker.clone()]);
        frame.relayout();
        let _ = frame.paint_with_damage();
        let label = crate::shell::i18n::t_args(
            "agent-marker-message",
            &[("task", marker.task.label.clone().into())],
        );
        activate(&mut frame, &label, &mut doc);
        let activated = frame
            .host()
            .take_accessible_agent_marker_activation()
            .expect("old paint must remain actionable after B starts");
        assert_eq!(activated.task.task_id, fixture.task_a);
        assert!(frame.host_mut().open_certified_agent_marker(activated));
        frame.pump_apps(&mut doc, &automerge::ROOT);
        frame.pump_apps(&mut doc, &automerge::ROOT);
        assert_eq!(
            editor(frame.host_mut())
                .unwrap()
                .private_draft_editor()
                .preedit,
            expected.preedit,
            "composition lost in reopened frame pump"
        );
        frame.relayout();
        let original_task = frame
            .host()
            .certified_marker_task(&room, "so.tilo.notes", "scribe", &original)
            .unwrap();
        assert_eq!(
            (original_task.task_id, original_task.run_epoch),
            (fixture.task_a.clone(), 1)
        );
        assert_eq!(
            frame
                .host_mut()
                .agent_quick_composer_mut()
                .marker
                .conversation_task(),
            Some((fixture.task_a.clone(), 1))
        );
        let retained_editor = editor(frame.host_mut()).unwrap().private_draft_editor();
        assert_eq!(retained_editor.text, expected.text, "private text changed");
        assert_eq!(
            (retained_editor.anchor, retained_editor.caret),
            (expected.anchor, expected.caret),
            "selection changed"
        );
        assert_eq!(
            retained_editor.preedit, expected.preedit,
            "composition changed"
        );
        assert_eq!(retained_editor.preedit_cursor, expected.preedit_cursor);
        capture(&mut frame, &pointer, &base.join("a-ended-b-running.png"));
        let _ = fixture.adopted.lock().unwrap().take();
        let label = crate::shell::i18n::t("agent-quick-open-conversation");
        activate(&mut frame, &label, &mut doc);
        wait_for(
            "real registered Chat writer must adopt the opaque private draft",
            || {
                frame.pump_apps(&mut doc, &automerge::ROOT);
                fixture.adopted_draft().is_some()
            },
        );
        let adopted = fixture.adopted_draft().unwrap();
        assert_eq!(adopted.workspace, room);
        assert!(
            adopted.target
                == runtime_collab::private_draft::Target::Task {
                    agent: "scribe".into(),
                    task: fixture.task_a.clone(),
                    run_epoch: 1,
                }
        );
        assert!(
            adopted.editor == expected,
            "private editor handoff lost selection or IME"
        );
    }
}
