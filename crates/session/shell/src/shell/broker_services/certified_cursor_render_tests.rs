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

/// One theme's retained frame, kept so a later reopen can prove what it preserved.
struct RenderedFrame {
    frame: FrameBuilder,
    pointer: vello::Scene,
    doc: automerge::AutoCommit,
    expected: ui::private_draft::Editor,
    base: std::path::PathBuf,
    marker: AccessibleAgentMarker,
}

/// The fixture-scoped inputs one rendered theme needs.
struct RenderInput<'a> {
    fixture: &'a CertifiedCursorFixture,
    tree: &'a crate::shell::state::WorkspaceTreeHandle,
    review: &'a crate::shell::task_review::NativeTaskReview,
    person: &'a str,
    original: &'a tilo_actions::SemanticInteraction,
    room: &'a str,
    name: &'a str,
    theme: ui::Theme,
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
        frames.push(render_theme(RenderInput {
            fixture: &fixture,
            tree: &tree,
            review: &review,
            person: &person,
            original: &original,
            room: &room,
            name,
            theme,
        }));
    }
    let b = fixture.finish_a_start_b();
    assert_ne!(b, fixture.task_a);
    wait_for("native review must observe B without retargeting A", || {
        review
            .snapshot(&room)
            .is_some_and(|snapshot| snapshot.is_ok_and(|s| s.tasks.iter().any(|t| t.id == b)))
    });
    frames.reverse();
    while let Some(rendered) = frames.pop() {
        reopen_frame(rendered, &fixture, &original, &room);
    }
    drop(frames);
    drop(fixture);
}

/// A retained frame with the admitted cursor marker it painted.
struct MarkerFrame {
    frame: FrameBuilder,
    pointer: vello::Scene,
    doc: automerge::AutoCommit,
    marker: AccessibleAgentMarker,
    bounds: Rect,
}

/// Render one theme's retained frame, its painted cursor and its private draft.
fn render_theme(input: RenderInput<'_>) -> RenderedFrame {
    let base =
        std::path::PathBuf::from("target/finish-verification/certified-cursor").join(input.name);
    let MarkerFrame {
        mut frame,
        pointer,
        mut doc,
        marker,
        bounds,
    } = open_marker_frame(input);
    let expected = capture_private_draft(&mut frame, &pointer, &mut doc, &base, bounds);
    RenderedFrame {
        frame,
        pointer,
        doc,
        expected,
        base,
        marker,
    }
}

fn open_marker_frame(input: RenderInput<'_>) -> MarkerFrame {
    let mut painted = paint_marker_frame(input);
    let interaction = painted.marker.interaction.clone();
    let room = painted.marker.room.clone();
    open_marker(
        &mut painted.frame,
        &mut painted.doc,
        &painted.marker,
        &interaction,
        &room,
    );
    painted
}

/// Paint the genuine broker-admitted cursor and publish it as an accessible marker.
fn paint_marker_frame(input: RenderInput<'_>) -> MarkerFrame {
    let RenderInput {
        fixture,
        tree,
        review,
        person,
        original,
        room,
        theme,
        ..
    } = input;
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
        FrameBuilder::with_native_fixture_inputs(tree.clone(), (WIDTH, HEIGHT), inputs).unwrap();
    frame
        .chrome_handle()
        .borrow_mut()
        .act_as(person.to_string());
    frame
        .host_mut()
        .attach_app_action_broker(fixture.actions.clone());
    frame.host_mut().attach_task_review(review.clone());
    frame
        .apply_native_interaction_for_test((*original).clone())
        .unwrap();
    frame.relayout();
    let mut presence = ui::PresenceCtx::empty();
    presence.apply_interaction((*original).clone()).unwrap();
    let mut pointer = vello::Scene::new();
    let mut text = ui::text_render::TextRenderer::new();
    let viewport = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
    let locus = Rect::new(360.0, 360.0, 520.0, 404.0);
    let geometry = presence
        .draw_agent_action_marker(
            &mut ui::paint::PaintCtx::new(&mut pointer, Affine::IDENTITY, &mut text, &theme),
            original,
            locus,
            viewport,
            "notes:release",
            ui::time::now(),
        )
        .expect("the genuine broker-admitted action must paint a visible cursor");
    assert!(geometry.bounds.area() > 0.0 && !pointer.encoding().is_empty());
    let task = frame
        .host()
        .certified_marker_task(room, "so.tilo.notes", "scribe", original)
        .expect("the broker must resolve the admitted original task");
    assert_eq!(task.task_id, fixture.task_a);
    let marker = AccessibleAgentMarker {
        connection: 71,
        surface: 72,
        app_id: "so.tilo.notes".into(),
        room: room.to_string(),
        agent: "scribe".into(),
        task,
        interaction: (*original).clone(),
        marker: geometry.bounds,
        locus,
        protected: locus.union(geometry.bounds),
        label: "Scribe".into(),
        description: presence.interaction_status_label(original),
        published_at: Instant::now(),
    };
    frame
        .host_mut()
        .publish_accessible_agent_markers(marker.connection, vec![marker.clone()]);
    frame.relayout();
    let _ = frame.paint_with_damage();
    MarkerFrame {
        frame,
        pointer,
        doc: automerge::AutoCommit::new(),
        marker,
        bounds: geometry.bounds,
    }
}

/// Open the painted marker through its semantic control and settle the composer.
fn open_marker(
    frame: &mut FrameBuilder,
    doc: &mut automerge::AutoCommit,
    marker: &AccessibleAgentMarker,
    original: &tilo_actions::SemanticInteraction,
    room: &str,
) {
    let label = crate::shell::i18n::t_args(
        "agent-marker-message",
        &[("task", marker.task.label.clone().into())],
    );
    assert!(
        !frame
            .host()
            .accessible_agent_marker_request_is_inert(&marker.action_target()),
        "painted marker unexpectedly inert; kind={:?}",
        frame
            .presence()
            .participant_kind(&ui::permissions::UserId(original.actor.id.clone()))
    );
    activate(frame, &label, doc);
    let selected = frame
        .host()
        .take_accessible_agent_marker_activation()
        .expect("semantic activation must retain the exact painted interaction");
    assert_eq!(&selected.interaction, original);
    assert_eq!(selected.task.task_id, marker.task.task_id);
    let source = frame.host().marker_action_session_source().unwrap();
    wait_for("provisioned identity custody must settle", || {
        source.identity_revision(room).is_some()
    });
    assert!(frame.host_mut().open_certified_agent_marker(selected));
    frame.pump_apps(doc, &automerge::ROOT);
    frame.relayout();
    let _ = frame.paint_with_damage();
    assert_eq!(
        frame
            .host_mut()
            .agent_quick_composer_mut()
            .marker
            .conversation_task(),
        Some((marker.task.task_id.clone(), 1))
    );
}

/// Type into the production private editor and prove only its cursor repainted.
fn capture_private_draft(
    frame: &mut FrameBuilder,
    pointer: &vello::Scene,
    doc: &mut automerge::AutoCommit,
    base: &std::path::Path,
    bounds: Rect,
) -> ui::private_draft::Editor {
    capture(frame, pointer, &base.join("a-ready.png"));
    let input = editor(frame.host_mut()).expect("production private editor is visible");
    assert!(input.perform_action(
        &ui::a11y::Action::SetValue("Keep the original references".into()),
        &[],
        &mut ui::widget::Edits::lend(doc),
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
    let positive = capture(frame, pointer, &base.join("a-private-draft.png"));
    let without = capture(
        frame,
        &vello::Scene::new(),
        &base.join("negative-no-pointer.png"),
    );
    assert!(
        cursor_pixels(&positive, &without, bounds) > 10,
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
    expected
}

/// Pixels the painted cursor owns: inside its bounds and absent from the negative control.
fn cursor_pixels(positive: &[u8], without: &[u8], bounds: Rect) -> usize {
    positive
        .chunks_exact(4)
        .zip(without.chunks_exact(4))
        .enumerate()
        .filter(|(index, (painted, absent))| {
            let index = u32::try_from(*index).unwrap();
            bounds.contains(vello::kurbo::Point::new(
                f64::from(index % WIDTH),
                f64::from(index / WIDTH),
            )) && painted != absent
        })
        .count()
}

/// Reopen the retained frame after B starts and prove it kept A's exact private draft.
fn reopen_frame(
    rendered: RenderedFrame,
    fixture: &CertifiedCursorFixture,
    original: &tilo_actions::SemanticInteraction,
    room: &str,
) {
    let RenderedFrame {
        mut frame,
        pointer,
        mut doc,
        expected,
        base,
        mut marker,
    } = rendered;
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
        &[("task", marker.task.label.into())],
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
        .certified_marker_task(room, "so.tilo.notes", "scribe", original)
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
    assert_chat_adopted_draft(&mut frame, &mut doc, fixture, &expected, room);
}

/// The real registered Chat writer must adopt the exact private draft A left open.
fn assert_chat_adopted_draft(
    frame: &mut FrameBuilder,
    doc: &mut automerge::AutoCommit,
    fixture: &CertifiedCursorFixture,
    expected: &ui::private_draft::Editor,
    room: &str,
) {
    let _ = fixture.adopted.lock().unwrap().take();
    let label = crate::shell::i18n::t("agent-quick-open-conversation");
    activate(frame, &label, doc);
    wait_for(
        "real registered Chat writer must adopt the opaque private draft",
        || {
            frame.pump_apps(doc, &automerge::ROOT);
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
        adopted.editor == *expected,
        "private editor handoff lost selection or IME"
    );
}
