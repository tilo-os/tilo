//! The editor in the toolkit's headless host: what typing re-measures, and how an agent's writes
//! show in a document.

use tilo_ui::State;
use tilo_ui::headless::Driver;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Container, Widget};
use winit::event::ElementState;

fn frame(root: Box<dyn Widget>) -> Driver {
    Driver::new_with_controlled_inputs(
        State::new(),
        root,
        (400, 240),
        Theme::dark(),
        tilo_ui::font_catalog::fixture_renderer(),
        tilo_ui::permissions::MemberId::person("portable-review"),
    )
}

/// The bounding box of the pixels two renders of one frame disagree on, inside `region` (x0, y0, x1, y1
/// in device pixels).
fn changed_pixels(
    before: &[u8],
    after: &[u8],
    width: u32,
    region: (u32, u32, u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for (index, (a, b)) in before
        .chunks_exact(4)
        .zip(after.chunks_exact(4))
        .enumerate()
    {
        if a == b {
            continue;
        }
        let index = u32::try_from(index).expect("a pixel index fits u32");
        let (x, y) = (index % width, index / width);
        if x < region.0 || y < region.1 || x >= region.2 || y >= region.3 {
            continue;
        }
        bbox = Some(match bbox {
            None => (x, y, x + 1, y + 1),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
        });
    }
    bbox
}

#[test]
fn typing_into_a_block_editor_re_measures_only_the_edited_line() {
    use tilo_rich_edit::BlockEditor;
    let mut editor = BlockEditor::from_markdown("alpha\n\nbeta\n\ngamma");
    editor.set_focus(true);
    let mut frame = frame(Box::new(editor));
    // The first key switches the interaction environment to keyboard, which
    // recomposes once (widgets may size differently per modality); settle
    // that before measuring an ordinary typed key.
    frame.type_text("w");
    frame.relayout();
    frame.set_retained_geometry_check(1);
    let ids = frame.layout_node_ids();
    let focused = frame.focused();
    assert!(
        focused.is_some(),
        "the editor's focused line owns keyboard focus"
    );

    let x = tilo_ui::keys::parse("x");
    frame.send_host_event(&tilo_ui::synthetic_input::key_input(
        &x,
        ElementState::Pressed,
    ));
    let typed = frame.last_relayout();
    frame.send_host_event(&tilo_ui::synthetic_input::key_input(
        &x,
        ElementState::Released,
    ));
    assert!(
        !frame.last_relayout().measured,
        "the key's release measures nothing: {:?}",
        frame.last_relayout()
    );
    assert!(
        !typed.rebuilt,
        "a character edit keeps the composed tree: {typed:?}"
    );
    assert!(
        typed.measured,
        "a character edit re-solves sizes: {typed:?}"
    );
    assert_eq!(
        typed.marked_leaves, 1,
        "only the edited line is re-measured: {typed:?}"
    );
    assert_eq!(
        frame.layout_node_ids(),
        ids,
        "node ids survive a retained relayout"
    );
    assert_eq!(
        frame.focused(),
        focused,
        "focus survives a retained relayout"
    );
    frame.verify_retained_geometry().unwrap();

    // Splitting the block adds a line but no widget: the block stack is one
    // measured leaf, so the editor restacks its lines in a relayout, and
    // the composed tree (its node ids, focus) survives. The retained
    // geometry check compares the result with a fresh composition.
    let rebuilt_before = frame.rebuilt_frames();
    let enter = tilo_ui::keys::parse("Enter");
    frame.send_host_event(&tilo_ui::synthetic_input::key_input(
        &enter,
        ElementState::Pressed,
    ));
    let split = frame.last_relayout();
    frame.send_host_event(&tilo_ui::synthetic_input::key_input(
        &enter,
        ElementState::Released,
    ));
    assert_eq!(
        frame.rebuilt_frames(),
        rebuilt_before,
        "inserting a block keeps the composition"
    );
    assert!(
        split.measured,
        "inserting a block re-solves sizes: {split:?}"
    );
    assert_eq!(
        frame.layout_node_ids(),
        ids,
        "a split adds no layout node and keeps the others"
    );
    assert_eq!(frame.focused(), focused, "focus survives a split");
    frame.verify_retained_geometry().unwrap();
}

fn written_interaction(start: u64, end: u64) -> tilo_actions::SemanticInteraction {
    let mut event = tilo_ui::headless::agent_acting("so.tilo.notes/note", "insert");
    event.actor = tilo_actions::Acting::agent("scribe", "portable-review");
    event.action.label = "Write".into();
    event.resource.label = "Note".into();
    event.anchor = tilo_actions::InteractionAnchor::Text {
        region: "body".into(),
        ranges: vec![tilo_actions::InteractionTextRange {
            block: "b1".into(),
            start,
            end,
        }],
    };
    event
}

fn note_frame(text: &str) -> Driver {
    use tilo_rich_edit::{BlockContent, BlockEditor};
    let mut editor = BlockEditor::new();
    // The agent's writes name it `b1`, the start of its id, as the document lets a caller.
    editor.present_blocks(&[BlockContent::paragraph("b1c2d3e4", text)]);
    let mut frame = frame(Box::new(
        Container::new()
            .with_padding(100.0)
            .with_child(Box::new(editor)),
    ));
    frame.relayout();
    frame
}

/// What an agent's action writes glows on exactly the characters it wrote — the same cells a person
/// typing those characters lights up — and not on the rest of the block.
#[test]
fn an_agents_written_text_glows_on_exactly_those_characters() {
    use tilo_ui::permissions::MemberId;
    use tilo_ui::presence::SelectionRange;
    let text = "The quick brown fox jumps";
    let key = tilo_rich_edit::block_editor::block_presence_key("b1c2d3e4");
    let (start, end) = (4u64, 9u64); // "quick"
    let bytes = (4usize, 9usize);

    let mut typed_frame = note_frame(text);
    let (width, _) = typed_frame.device_size();
    let plain = typed_frame.render_rgba();
    typed_frame.update_presence(|presence| {
        presence.record_peer_typing(
            MemberId::person("reviewer"),
            key,
            SelectionRange {
                start: bytes.0,
                end: bytes.1,
            },
            tilo_ui::time::now(),
        );
        true
    });
    typed_frame.poll_frame().unwrap();
    typed_frame.relayout();
    let whole = (0, 0, width, u32::MAX);
    let typed = changed_pixels(&plain, &typed_frame.render_rgba(), width, whole)
        .expect("a person's typing shows, or there is nothing to compare against");

    let mut agent_frame = note_frame(text);
    let event = written_interaction(start, end);
    let at = tilo_ui::time::now()
        .checked_sub(tilo_ui::time::Duration::from_secs(1))
        .expect("the clock is later than a second ago");
    let mut label = String::new();
    agent_frame.update_presence(|presence| {
        presence
            .names
            .insert(MemberId::agent("scribe", "portable-review"), "Ada".into());
        let applied = presence
            .apply_interaction_at(event.clone(), at)
            .expect("a well-formed interaction applies");
        label = presence.interaction_status_label(&event);
        applied
    });
    assert_eq!(
        label, "Ada: Writing in Note",
        "the line names the kind of work and the record it writes in"
    );
    assert!(
        agent_frame.semantic_locus(&event).is_none(),
        "a text anchor is drawn in the document, not on a control"
    );
    agent_frame.poll_frame().unwrap();
    agent_frame.relayout();
    let drawn = agent_frame.render_rgba();
    if std::env::var_os("TILO_UI_AGENT_MARKER_SHOTS").is_some() {
        let dir = agent_marker_shots();
        std::fs::create_dir_all(&dir).unwrap();
        typed_frame.render_png(&dir.join("note-person-typing.png"));
        agent_frame.render_png(&dir.join("note-agent-caret-and-glow.png"));
        agent_frame.set_theme(tilo_ui::theme::Theme::light());
        agent_frame.render_png(&dir.join("note-agent-caret-and-glow-light.png"));
    }
    let line = (0, typed.1, width / 2, typed.3);
    let glow = changed_pixels(&plain, &drawn, width, line).expect("the agent's write glows");
    assert!(
        glow.0.abs_diff(typed.0) <= 1,
        "the characters before the write do not glow: {glow:?} vs typing {typed:?}"
    );
    assert!(
        glow.2 >= typed.2 && glow.2 <= typed.2 + 4,
        "the glow ends on the written characters, with its caret: {glow:?} vs typing {typed:?}"
    );
}

/// An agent writing in a note has its live caret where its write ends, and its action caret there too:
/// it wears one tag, the one saying who it is and what it is doing, never its name a second time over
/// the same words.
#[test]
fn an_agent_writing_wears_one_tag_at_its_caret() {
    use tilo_ui::permissions::MemberId;
    let text = "The quick brown fox jumps";
    let key = tilo_rich_edit::block_editor::block_presence_key("b1c2d3e4");
    let mut frame = note_frame(text);
    let event = written_interaction(4, 9);
    let at = tilo_ui::time::now()
        .checked_sub(tilo_ui::time::Duration::from_secs(1))
        .expect("the clock is later than a second ago");
    frame.update_presence(|presence| {
        presence
            .names
            .insert(MemberId::agent("scribe", "portable-review"), "Ada".into());
        presence.set_caret(&key, MemberId::agent("scribe", "portable-review"), 9);
        presence
            .apply_interaction_at(event.clone(), at)
            .expect("a well-formed interaction applies")
    });
    frame.poll_frame().unwrap();
    frame.relayout();
    let _ = frame.render_rgba();
    let tags: Vec<String> = frame
        .presence()
        .caret_tags()
        .into_iter()
        .filter(|tag| tag.user == MemberId::agent("scribe", "portable-review"))
        .map(|tag| tag.label)
        .collect();
    assert_eq!(
        tags,
        vec!["Ada: wrote".to_owned()],
        "one tag, saying who and what it did to the text"
    );
    // The person typing in that paragraph puts the agent's write away.
    frame.click_at(112.0, 110.0);
    frame.type_text("x");
    frame.poll_frame().unwrap();
    frame.relayout();
    let _ = frame.render_rgba();
    assert!(
        frame
            .presence()
            .caret_tags()
            .iter()
            .all(|tag| tag.label != "Ada: wrote"),
        "the agent's write is no longer drawn"
    );
}

/// Where a test writes captures a reviewer reads: the build's target directory, so a remote build can
/// hand them back (`--fetch`).
fn agent_marker_shots() -> std::path::PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => std::path::PathBuf::from(dir).join("agent-marker"),
        None => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target")
            .join("agent-marker"),
    }
}
