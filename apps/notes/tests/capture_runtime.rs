use crate::component_support::*;

#[test]
fn in_note_find_and_handle_menu_captures_use_real_input() {
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/notes-review/find");
    std::fs::create_dir_all(&directory).unwrap();
    let captures = [
        ("find", "in-note-find.png"),
        ("block_type_menu", "block-type-menu.png"),
    ]
    .map(|(scenario, name)| crate::captures::Capture {
        label: scenario.to_owned(),
        scenario: Some(scenario),
        viewport: (1000, 700),
        path: directory.join(name),
    });
    crate::captures::render_all(&captures);
}

#[test]
fn canonical_markdown_and_title_affordance_captures_reach_real_mounted_states() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/notes-review/canonical-affordances");
    std::fs::create_dir_all(&directory).unwrap();
    let captures = [
        ("slash", (1000, 700)),
        ("slash_applied", (390, 844)),
        ("heading", (390, 844)),
        ("quote", (1000, 700)),
        ("title_edit", (1000, 700)),
    ]
    .map(|(scenario, viewport)| crate::captures::Capture {
        label: scenario.to_owned(),
        scenario: Some(scenario),
        viewport,
        path: directory.join(format!("{scenario}.png")),
    });
    crate::captures::render_all(&captures);
}

fn render_canonical_document_feature(scenario: &str, viewport: (u32, u32)) {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/notes-review/canonical-document-features");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{scenario}.png"));
    notes::shots::render_to_path(viewport, &path, Some(scenario), None)
        .unwrap_or_else(|error| panic!("{scenario}: {error:?}"));
    assert!(
        std::fs::read(&path)
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
}

macro_rules! canonical_document_feature_captures {
    ($(($test:ident, $scenario:literal, $viewport:expr)),+ $(,)?) => {
        $(
            #[test]
            fn $test() {
                render_canonical_document_feature($scenario, $viewport);
            }
        )+
    };
}

// Keep each genuine full-frame render under its own nextest timeout. Capture
// runtime tests reserve the complete runner in `.config/nextest.toml`, so this
// finer reporting does not increase renderer concurrency.
canonical_document_feature_captures!(
    (
        canonical_document_feature_paragraph_click_capture,
        "paragraph_click",
        (740, 700)
    ),
    (
        canonical_document_feature_mixed_outline_capture,
        "mixed_outline",
        (740, 800)
    ),
    (
        canonical_document_feature_recipes_capture,
        "recipes",
        (1000, 700)
    ),
    (
        canonical_document_feature_formula_capture,
        "formula",
        (1000, 800)
    ),
    (
        canonical_document_feature_transclusion_capture,
        "transclusion",
        (1000, 700)
    ),
    (
        canonical_document_feature_external_link_capture,
        "external_link",
        (740, 700)
    ),
    (
        canonical_document_feature_table_capture,
        "table",
        (1000, 700)
    ),
    (
        canonical_document_feature_large_table_capture,
        "table_large",
        (1000, 800)
    ),
    (
        canonical_document_feature_scrolled_large_table_capture,
        "table_large_scrolled",
        (390, 844)
    ),
    (
        canonical_document_feature_table_cell_edit_capture,
        "table_cell_edit",
        (1000, 700)
    ),
    (
        canonical_document_feature_image_capture,
        "image",
        (1000, 700)
    ),
    (
        canonical_document_feature_image_edit_capture,
        "image_edit",
        (1000, 700)
    ),
    (
        canonical_document_feature_scrolled_document_capture,
        "scrolled",
        (390, 844)
    ),
);

#[test]
fn canonical_navigation_and_overlay_captures_use_real_input_paths() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/notes-review/canonical-navigation-overlays");
    std::fs::create_dir_all(&directory).unwrap();
    let captures = [
        ("drawer", (390, 844)),
        ("drag", (1000, 700)),
        ("block_drag", (1000, 700)),
        ("menu", (1000, 700)),
        ("sidebar_hover", (1000, 700)),
        ("note_menu", (1000, 700)),
        ("rename", (1000, 700)),
        ("rename_commit", (1000, 700)),
        ("shortcuts", (1000, 700)),
        ("shortcuts_bottom", (390, 844)),
        ("code_group_drag", (1000, 700)),
        ("code_group_drop", (1000, 700)),
        ("sidebar_scroll", (390, 844)),
        ("presence", (1000, 700)),
        ("status", (1000, 700)),
        ("saving", (1000, 700)),
        ("save_error", (1000, 700)),
        ("import_success", (1000, 700)),
    ]
    .map(|(scenario, viewport)| crate::captures::Capture {
        label: scenario.to_owned(),
        scenario: Some(scenario),
        viewport,
        path: directory.join(format!("{scenario}.png")),
    });
    crate::captures::render_all(&captures);
}

#[test]
fn canonical_save_failure_capture_uses_the_real_record_writer() {
    let output = tempfile::tempdir().unwrap();
    let path = output.path().join("save_error.png");
    notes::capture::render_to_path((1000, 700), &path, Some("save_error"), None)
        .unwrap_or_else(|error| panic!("save_error: {error:?}"));
    assert!(
        std::fs::read(path)
            .unwrap()
            .starts_with(b"\x89PNG\r\n\x1a\n")
    );
}

#[test]
fn creating_then_visiting_another_note_keeps_creation_reachable_by_ctrl_z() {
    let fixture = LegacyFixture::rich();
    let checkout = imported(&fixture);
    let initial = fixture
        .records
        .read(&checkout.id, |doc| {
            notes::doc::NotesDoc::list_pages(doc).len()
        })
        .unwrap();
    let mut live = driver(&fixture, 225, &checkout);

    let new_note = semantic_rect(&live, "new-note").center();
    live.click_at(new_note.x, new_note.y);
    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| notes::doc::NotesDoc::list_pages(doc)
                .len())
            .unwrap(),
        initial + 1,
        "the visible new-note affordance creates one canonical note"
    );

    let other = page_rect(&live, fixture.root).center();
    live.click_at(other.x, other.y);
    live.set_modifiers(&["ctrl".into()]);
    live.key("z");
    live.set_modifiers(&[]);

    assert_eq!(
        fixture
            .records
            .read(&checkout.id, |doc| notes::doc::NotesDoc::list_pages(doc)
                .len())
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
    let prepared = notes::capture::prepare_fixture(Some("heading"), runner_inputs()).unwrap();
    let component = notes::definition()
        .instantiate(prepared.context, prepared.target, prepared.restore)
        .unwrap();
    let mut driver =
        ui::headless::Driver::from_component(component, (390, 844), prepared.runner_inputs)
            .unwrap();
    notes::capture::drive_fixture(&mut driver, Some("heading")).unwrap();
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
    close_driver(&mut driver);
}

fn text_node(driver: &ui::headless::Driver, target: &str) -> ui::a11y::AccessNode {
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
    use notes::doc::NotesDoc;
    let source = "message = \"Long Python strings remain completely readable inside a narrow note without adding any source line breaks.\"";
    for scale in [1.0, 2.0] {
        let f = LegacyFixture::rich();
        f.write_page(
            f.child,
            format!("```python\n{source}\nprint(message)\n```\n\nFollowing paragraph\n").as_bytes(),
        );
        let checkout = imported(&f);
        let before = f
            .records
            .read(&checkout.id, |doc| {
                NotesDoc::page_block_snapshots(doc, f.child)
            })
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
        invoke(
            &mut live,
            225,
            notes::actions::page_resource_id(f.child),
            "open",
            None,
        );
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
                .read(&checkout.id, |doc| NotesDoc::page_block_snapshots(
                    doc, f.child
                ))
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
            .read(&checkout.id, |doc| {
                NotesDoc::page_block_snapshots(doc, f.child)
            })
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
            .read(&checkout.id, |doc| {
                NotesDoc::page_block_snapshots(doc, f.child)
            })
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
    let prepared =
        notes::capture::prepare_fixture(Some("fenced_code_opening"), runner_inputs()).unwrap();
    let component = notes::definition()
        .instantiate(prepared.context, prepared.target, prepared.restore)
        .unwrap();
    let mut driver =
        ui::headless::Driver::from_component(component, (1000, 700), prepared.runner_inputs)
            .unwrap();
    notes::capture::drive_fixture(&mut driver, Some("fenced_code_opening")).unwrap();
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
    close_driver(&mut driver);
}

#[test]
#[allow(
    clippy::unicode_not_nfc,
    reason = "test deliberately includes a decomposed accent to exercise grapheme selection"
)]
fn accessible_block_text_edits_and_grapheme_selection_use_the_canonical_history_path() {
    use notes::doc::NotesDoc;
    let f = LegacyFixture::rich();
    f.write_page(f.child, "a👩‍👩‍👧‍👦éz\n\n```python\nprint(1)\n```\n".as_bytes());
    let checkout = imported(&f);
    let before = f
        .records
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, f.child)
        })
        .unwrap();
    let mut live = driver(&f, 224, &checkout);
    invoke(
        &mut live,
        224,
        notes::actions::page_resource_id(f.child),
        "open",
        None,
    );
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
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, f.child)
        })
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
            .read(&checkout.id, |doc| NotesDoc::page_block_snapshots(
                doc, f.child
            ))
            .unwrap()[1],
        (before[1].0.clone(), "print(2)".to_owned())
    );
    live.set_modifiers(&["ctrl".into()]);
    live.key("z");
    live.set_modifiers(&[]);
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| NotesDoc::page_block_snapshots(
                doc, f.child
            ))
            .unwrap()[1],
        before[1]
    );
    close_driver(&mut live);
    drop(live);
    let f = f.reopen();
    f.records.try_open_record(&checkout).unwrap();
    let reopened = f
        .records
        .read(&checkout.id, |doc| {
            NotesDoc::page_block_snapshots(doc, f.child)
        })
        .unwrap();
    assert_eq!(reopened[0], changed[0]);
    assert_eq!(reopened[1], before[1]);
}
