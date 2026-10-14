// The host's accessibility walk over a long block-editor document, driven as
// a host drives an app it shows: input through `surface_input`, frames
// through `surface_frame`, then the refresh the host reads. A frame that
// changed nothing walks no widgets; a change publishes on its own frame.

/// A long document in a scroller, in a window too short to show it: an app
/// worker with its own wake scope, as a hosted app runs, so a frame with no
/// wake pending synchronizes nothing.
fn long_document_app() -> (App, Region) {
    crate::i18n::I18nManager::init("en-US");
    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    let body = (0..48)
        .map(|index| {
            format!(
                "Paragraph {index}: {}\n\n",
                "ordinary notebook prose that wraps across the editor. ".repeat(3)
            )
        })
        .collect::<String>();
    let root = crate::widget::Scrollable::new_vertical()
        .with_child(Box::new(crate::widget::BlockEditor::from_markdown(&body)));
    let app = App::from_scoped_parts_with_controlled_inputs(scope, State::new(), Box::new(root));
    let region = Region {
        size: (900, 480),
        redraw: Cell::new(false),
    };
    (app, region)
}

/// One host frame, as `EmbeddedApp::draw` runs it: app frames until the app
/// is at rest, then the accessibility walk the host reads.
fn host_frame(app: &mut App, region: &Region) -> crate::a11y_tree::TreeDelta {
    let mut scene = Scene::new();
    for _ in 0..3 {
        scene.reset();
        if !app.surface_frame(region, &mut scene) || !app.unsettled() {
            break;
        }
    }
    app.refresh_semantic_tree()
}

/// The text field of the block that reads `prefix`, as the tree has it.
fn block_text<'a>(
    tree: &'a crate::a11y_tree::SemanticTree,
    prefix: &str,
) -> &'a crate::a11y_tree::SemanticNode {
    tree.iter()
        .find(|node| {
            node.name.as_deref() == Some("Block text")
                && node
                    .value
                    .as_deref()
                    .is_some_and(|value| value.starts_with(prefix))
        })
        .unwrap_or_else(|| {
            let nodes: Vec<_> = tree
                .iter()
                .take(40)
                .map(|node| (node.role, node.name.clone(), node.value.clone()))
                .collect();
            panic!("no block text reads {prefix:?}: {nodes:#?}")
        })
}

/// The app at rest after its first frames, the first block focused by a
/// click on it, as a person starts typing.
fn settled_with_first_block_focused() -> (App, Region) {
    let (mut app, region) = long_document_app();
    for _ in 0..3 {
        host_frame(&mut app, &region);
    }
    let bounds = block_text(app.semantic_tree(), "Paragraph 0:")
        .bounds
        .expect("the first block has bounds");
    let at = bounds.center();
    for event in [
        crate::synthetic_input::cursor_moved(at.x, at.y),
        crate::synthetic_input::mouse_press(),
        crate::synthetic_input::mouse_release(),
    ] {
        app.surface_input(&region, event, false);
    }
    host_frame(&mut app, &region);
    assert!(
        block_text(app.semantic_tree(), "Paragraph 0:")
            .state
            .focused,
        "the click focuses the first block, and its frame says so"
    );
    for _ in 0..3 {
        host_frame(&mut app, &region);
    }
    (app, region)
}

#[test]
fn unchanged_frames_walk_no_widgets() {
    let (mut app, region) = settled_with_first_block_focused();
    let builds = app.semantic_tree_builds_for_test();
    let nodes = app.semantic_tree().len();
    for _ in 0..20 {
        let delta = host_frame(&mut app, &region);
        assert!(delta.is_empty(), "an unchanged frame publishes {delta:?}");
    }
    assert_eq!(
        app.semantic_tree_builds_for_test(),
        builds,
        "twenty frames that changed nothing rebuilt the semantic tree"
    );
    assert_eq!(app.semantic_tree().len(), nodes);
}

#[test]
fn a_keystroke_publishes_its_block_on_its_own_frame() {
    let (mut app, region) = settled_with_first_block_focused();
    let builds = app.semantic_tree_builds_for_test();
    let id = block_text(app.semantic_tree(), "Paragraph 0:").id.clone();
    let key = winit::keyboard::Key::Character("Z".into());
    app.surface_input(
        &region,
        crate::synthetic_input::key_input(&key, ElementState::Pressed),
        false,
    );
    let delta = host_frame(&mut app, &region);
    assert_eq!(
        app.semantic_tree_builds_for_test(),
        builds + 1,
        "the keystroke's frame walks the widgets once"
    );
    let typed = app
        .semantic_tree()
        .node(&id)
        .and_then(|node| node.value.clone())
        .expect("the edited block keeps its node");
    assert!(
        typed.contains('Z'),
        "the frame that shows the key publishes it: {typed:?}"
    );
    assert!(
        delta.updated.contains(&id),
        "the edited block is in the frame's delta: {delta:?}"
    );
    // The key changed one block: its row is published, not the document.
    assert!(
        delta.added.is_empty() && delta.removed.is_empty() && delta.updated.len() <= 4,
        "a keystroke republished more than its block: {delta:?}"
    );
    // Released, and at rest again: nothing more to publish or walk.
    app.surface_input(
        &region,
        crate::synthetic_input::key_input(&key, ElementState::Released),
        false,
    );
    host_frame(&mut app, &region);
    let builds = app.semantic_tree_builds_for_test();
    for _ in 0..5 {
        assert!(host_frame(&mut app, &region).is_empty());
    }
    assert_eq!(app.semantic_tree_builds_for_test(), builds);
}

#[test]
fn a_scroll_publishes_the_bounds_it_moved_on_its_own_frame() {
    let (mut app, region) = settled_with_first_block_focused();
    let id = block_text(app.semantic_tree(), "Paragraph 3:").id.clone();
    let before = app
        .semantic_tree()
        .node(&id)
        .and_then(|node| node.bounds)
        .expect("a block has bounds");
    app.surface_input(
        &region,
        crate::synthetic_input::cursor_moved(300.0, 240.0),
        false,
    );
    host_frame(&mut app, &region);
    assert!(app.surface_input(
        &region,
        crate::synthetic_input::mouse_wheel(0.0, -2.0),
        false
    ));
    let delta = host_frame(&mut app, &region);
    let after = app
        .semantic_tree()
        .node(&id)
        .and_then(|node| node.bounds)
        .expect("the block keeps its bounds");
    assert!(
        after.y0 < before.y0,
        "the frame that shows the scroll publishes where the block went: {before:?} -> {after:?}"
    );
    assert!(
        delta.updated.contains(&id),
        "the moved block is in the frame's delta: {delta:?}"
    );
}

#[test]
fn a_focus_move_publishes_on_its_own_frame() {
    let (mut app, region) = settled_with_first_block_focused();
    let first = block_text(app.semantic_tree(), "Paragraph 0:").id.clone();
    let second = block_text(app.semantic_tree(), "Paragraph 1:");
    let (second, at) = (
        second.id.clone(),
        second.bounds.expect("the second block has bounds").center(),
    );
    for event in [
        crate::synthetic_input::cursor_moved(at.x, at.y),
        crate::synthetic_input::mouse_press(),
        crate::synthetic_input::mouse_release(),
    ] {
        app.surface_input(&region, event, false);
    }
    let delta = host_frame(&mut app, &region);
    let focused = |id: &crate::a11y_tree::SemanticId| {
        app.semantic_tree()
            .node(id)
            .is_some_and(|node| node.state.focused)
    };
    assert!(
        focused(&second) && !focused(&first),
        "the frame that moves focus publishes where it went"
    );
    assert!(
        delta.updated.contains(&first) && delta.updated.contains(&second),
        "both blocks are in the frame's delta: {delta:?}"
    );
}
