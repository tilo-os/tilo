use crate::component_support::*;

#[test]
fn creating_then_visiting_another_note_keeps_creation_reachable_by_ctrl_z() {
    let fixture = NotebookFixture::rich();
    let checkout = notebook(&fixture);
    let initial = fixture
        .records
        .read(&checkout.id, |doc| notes::read::list_pages(doc).len())
        .unwrap();
    let mut live = driver(&fixture, 225, &checkout);

    let new_note = semantic_rect(&live, "new-note").center();
    live.click_at(new_note.x, new_note.y);
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| notes::read::list_pages(doc).len())
            .unwrap(),
        initial + 1,
        "the visible new-note affordance creates one canonical note"
    );
    // The new note opens to be named; Enter keeps its name.
    live.key("Enter");

    let other = page_rect(&live, fixture.root).center();
    live.click_at(other.x, other.y);
    live.set_modifiers(&["ctrl".into()]);
    live.key("z");
    live.set_modifiers(&[]);

    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| notes::read::list_pages(doc).len())
            .unwrap(),
        initial,
        "switching notes must not strand the creation outside action history"
    );
    close_driver(&mut live);
}

#[test]
fn short_narrow_note_keeps_its_title_and_metadata_above_the_caret() {
    fn retained<'a>(
        node: &'a ui::headless::InspectionNode,
        id: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some(id) {
            return Some(node);
        }
        node.children.iter().find_map(|child| retained(child, id))
    }
    let mut app = ui::framework::testing::TestApp::<notes::NotesShell>::capture_at(
        Some("heading"),
        (390, 844),
    )
    .unwrap();
    let driver = app.harness();
    driver
        .expect(&ui::headless::SemanticExpectation::named("note_title").value("Writing"))
        .unwrap();
    let snapshot = driver.inspection();
    let title = retained(&snapshot.root, "note_title").unwrap();
    assert!(
        title.rect[1] >= 32.0 && title.rect[1] + title.rect[3] < 180.0,
        "a two-line note must retain its pinned title band: {:?}",
        title.rect
    );
}

fn text_node(driver: &ui::headless::FrameHarness, target: &str) -> ui::a11y::AccessNode {
    fn semantic(node: &ui::a11y::AccessNode, target: &str) -> Option<ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(target) {
            return Some(node.clone());
        }
        node.children
            .iter()
            .find_map(|child| semantic(child, target))
    }
    fn retained(node: &ui::headless::InspectionNode, target: &str) -> Option<ui::a11y::AccessNode> {
        node.semantics
            .as_ref()
            .and_then(|node| semantic(node, target))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| retained(child, target))
            })
    }
    retained(&driver.inspection().root, target).expect("real code text must be inspectable")
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "regression checks both text scales, source preservation, geometry, and rendering together"
)]
fn wrapped_code_keeps_source_lines_and_caret_geometry_at_normal_and_large_text_sizes() {
    use notes::read;
    let source = "message = \"Long Python strings remain completely readable inside a narrow note without adding any source line breaks.\"";
    for scale in [1.0, 2.0] {
        let f = NotebookFixture::rich();
        f.write_page(
            f.child,
            format!("```python\n{source}\nprint(message)\n```\n\nFollowing paragraph\n").as_bytes(),
        );
        let checkout = notebook(&f);
        let before = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap();
        let mut live = driver(&f, 225, &checkout);
        live.set_theme(
            if (scale - 1.0_f32).abs() < f32::EPSILON {
                ui::Theme::dark()
            } else {
                ui::Theme::light()
            }
            .with_text_scale(scale),
        );
        live.resize((390, 1500)).unwrap();
        open_note(&mut live, 225, f.child, None);
        let target = format!("notes.block-text:{}", before[0].0);
        let next_target = format!("notes.block-text:{}", before[1].0);
        let first = text_node(&live, &target);
        assert_eq!(first.value.as_deref(), Some(source));
        let bounds = first.bounds.unwrap();
        let next_bounds = text_node(&live, &next_target).bounds.unwrap();
        assert!(
            bounds.y1 - bounds.y0 > 42.0 * f64::from(scale),
            "a long source line must expand into visible rows: {bounds:?}"
        );
        assert!(
            next_bounds.y0 >= bounds.y1,
            "the next source line must not overlap wrapped text"
        );
        let original_revision = revision(&f, &checkout);
        live.click_at(bounds.x0 + 11.0, bounds.y0 + 2.0);
        live.key("ArrowDown");
        let moved = text_node(&live, &target).text_selection.unwrap().focus;
        assert!(
            moved > 0 && moved < source.len(),
            "Down should navigate the next visual row inside this source line"
        );
        live.key("ArrowUp");
        assert_eq!(
            text_node(&live, &target).bounds,
            Some(bounds),
            "arrow navigation must not move code vertically"
        );
        assert_eq!(revision(&f, &checkout), original_revision);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
                .unwrap(),
            before
        );
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/notes-review/wrapped-code-{scale}.png"
        ));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        live.render_png(&out);
        live.perform_semantic_named(
            "block_editor",
            Some(&target),
            &ui::a11y::Action::SetSelection {
                anchor: source.len(),
                focus: source.len(),
            },
        )
        .unwrap();
        live.set_modifiers(&["shift".into()]);
        live.key("Enter");
        live.set_modifiers(&[]);
        let split = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap();
        assert_eq!(split[0], before[0]);
        assert_eq!(split[1].1, "", "Shift+Enter creates a blank source row");
        assert_eq!(split[2], before[1]);
        live.set_modifiers(&["ctrl".into()]);
        live.key("Enter");
        live.set_modifiers(&[]);
        live.type_text("Outside code");
        let after = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap();
        assert_eq!(after[0], before[0]);
        assert_eq!(
            after.last().unwrap(),
            &(
                before[2].0.clone(),
                "Outside codeFollowing paragraph".to_owned()
            )
        );
        close_driver(&mut live);
    }
}

#[test]
#[allow(
    clippy::items_after_statements,
    reason = "inspection predicates are local to the profiling assertion that consumes them"
)]
fn canonical_typing_records_real_frame_cost_and_visible_content() {
    let mut app = ui::framework::testing::TestApp::<notes::NotesShell>::capture_at(
        Some("fenced_code_opening"),
        (1000, 700),
    )
    .unwrap();
    let driver = app.harness();
    driver.key("Enter");
    driver.begin_profiling();
    let started = ui::time::now();
    driver.type_text("print(1)");
    let typing_us = started.elapsed().as_micros();
    let samples = driver.take_profile_samples();
    assert!(samples.len() >= 8, "every character must drive real frames");
    fn contains(node: &ui::a11y::AccessNode) -> bool {
        node.value.as_deref() == Some("print(1)") || node.children.iter().any(contains)
    }
    fn retained(node: &ui::headless::InspectionNode) -> bool {
        node.semantics.as_ref().is_some_and(contains) || node.children.iter().any(retained)
    }
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/notes-review/canonical-typing-profile.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(
        out,
        serde_json::to_vec_pretty(&serde_json::json!({"typing_us": typing_us, "frames": samples}))
            .unwrap(),
    )
    .unwrap();
    assert!(
        retained(&driver.inspection().root),
        "typed code must be visible in the mounted component: {}",
        driver.inspection_json().unwrap()
    );
}

#[test]
#[allow(
    clippy::unicode_not_nfc,
    reason = "test deliberately includes a decomposed accent to exercise grapheme selection"
)]
fn accessible_block_text_edits_and_grapheme_selection_use_the_canonical_history_path() {
    use notes::read;
    let f = NotebookFixture::rich();
    f.write_page(f.child, "a👩‍👩‍👧‍👦éz\n\n```python\nprint(1)\n```\n".as_bytes());
    let checkout = notebook(&f);
    let before = f
        .records
        .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
        .unwrap();
    let mut live = driver(&f, 224, &checkout);
    open_note(&mut live, 224, f.child, None);
    let target = format!("notes.block-text:{}", before[0].0);
    let code_target = format!("notes.block-text:{}", before[1].0);
    let initial_revision = revision(&f, &checkout);
    live.perform_semantic_named(
        "block_editor",
        Some(&target),
        &ui::a11y::Action::SetSelection {
            anchor: 1,
            focus: 3,
        },
    )
    .unwrap();
    assert_eq!(
        revision(&f, &checkout),
        initial_revision,
        "selection is presentation only"
    );
    live.type_text("X");
    let changed = f
        .records
        .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
        .unwrap();
    assert_eq!(changed[0], (before[0].0.clone(), "aXz".to_owned()));
    let prior_edit = revision(&f, &checkout);
    live.perform_semantic_named(
        "block_editor",
        Some(&code_target),
        &ui::a11y::Action::SetValue("print(2)".into()),
    )
    .unwrap();
    assert_eq!(revision(&f, &checkout), prior_edit + 1);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap()[1],
        (before[1].0.clone(), "print(2)".to_owned())
    );
    live.set_modifiers(&["ctrl".into()]);
    live.key("z");
    live.set_modifiers(&[]);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap()[1],
        before[1]
    );
    close_driver(&mut live);
    drop(live);
    let f = f.reopen();
    f.records.try_open_record(&checkout).unwrap();
    let reopened = f
        .records
        .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
        .unwrap();
    assert_eq!(reopened[0], changed[0]);
    assert_eq!(reopened[1], before[1]);
}
