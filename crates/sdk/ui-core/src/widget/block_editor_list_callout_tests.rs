//! List markers, list spacing and callouts as the Notes editor lays them
//! out and paints them: wrapping lines, compact prose spacing.

use super::*;

/// An editor configured as Notes configures its page.
fn page(blocks: Vec<Block>) -> BlockEditor {
    let mut editor = BlockEditor::new()
        .with_wrap_lines(true)
        .with_compact_paragraph_spacing(true);
    editor.set_blocks(blocks);
    editor
}

fn lay_out_and_draw(editor: &mut BlockEditor, theme: &Theme) -> (Scene, TextRenderer) {
    let width = 480.0;
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(theme.text_scale);
    let mut tree = TaffyTree::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = editor.build_layout(&mut tree, theme, &mut nodes);
    let root = tree
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(width),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    tree.compute_layout_with_measure(
        root,
        Size {
            width: taffy::AvailableSpace::Definite(width),
            height: taffy::AvailableSpace::MaxContent,
        },
        |known, available, node, _, _| {
            nodes.get(&node).map_or(Size::ZERO, |widget| {
                // The pointers are the stable widget leaves build_layout registered.
                unsafe { (**widget).measure(known, available, &mut renderer) }
            })
        },
    )
    .unwrap();
    editor.apply_layout(&tree, node, Point::ORIGIN);
    let mut scene = Scene::new();
    editor.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        theme,
        &PresenceCtx::empty(),
    );
    (scene, renderer)
}

/// Where block `index`'s text starts, and the content edge its prefix
/// column starts at.
fn text_start_and_edge(editor: &BlockEditor, index: usize) -> (f64, f64) {
    let block = &editor.blocks[index];
    let line = block.line.get_rect();
    let pad = block.line.text_inset(&editor.layout_theme);
    let gutter = prefix_gutter_for(&block.kind) * editor.text_scale;
    (line.x0 + pad, content_edge(line, gutter, pad))
}

/// Whether `scene` fills anything in exactly `color`.
fn scene_fills(scene: &Scene, color: vello::peniko::Color) -> bool {
    use vello_encoding::{DrawMonoid, DrawTag, Monoid};
    let expected = vello_encoding::DrawColor::from(color).rgba;
    let encoding = scene.encoding();
    let mut offset = 0;
    for tag in &encoding.draw_tags {
        let words = usize::try_from(DrawMonoid::new(*tag).scene_offset).unwrap();
        if *tag == DrawTag::COLOR && encoding.draw_data.get(offset) == Some(&expected) {
            return true;
        }
        offset += words;
    }
    false
}

fn access_names(node: &crate::a11y::AccessNode, out: &mut Vec<String>) {
    if let Some(name) = &node.name {
        out.push(name.clone());
    }
    for child in &node.children {
        access_names(child, out);
    }
}

#[test]
fn bullets_numbers_and_boxes_share_one_marker_column() {
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let long = "A bullet long enough to wrap onto a second row of the page, \
                    and then onto a third one as well, to see where its dot sits";
        let mut editor = page(vec![
            Block::bullet(0, long),
            Block::numbered(0, "First"),
            Block::checklist(0, false, "Task"),
        ]);
        let (_, mut renderer) = lay_out_and_draw(&mut editor, &theme);
        let x_height_em = editor.marker_x_height.as_ref().map_or(0.5, |(_, em)| *em);

        let (bullet_text, bullet_edge) = text_start_and_edge(&editor, 0);
        let (number_text, number_edge) = text_start_and_edge(&editor, 1);
        let (box_text, box_edge) = text_start_and_edge(&editor, 2);
        assert!((bullet_text - number_text).abs() < 0.01, "scale {scale}");
        assert!((bullet_text - box_text).abs() < 0.01, "scale {scale}");

        let bullet_line = &editor.blocks[0].line;
        let disc =
            list_marker::bullet_disc(bullet_line, bullet_edge, &theme, &mut renderer, x_height_em);
        let font_px = f64::from(bullet_line.line_style(theme.text_scale).size);
        assert!(
            (disc.radius * 2.0 / font_px - list_marker::BULLET_DIAMETER_EM).abs() < 1e-6,
            "a bullet is a typographic bullet's size: {} at {font_px} px",
            disc.radius * 2.0
        );
        // On the first row's x-height: below the row's centre, above its
        // baseline, though the item wraps onto further rows.
        let (top, row) = bullet_line.first_row_band();
        assert!(
            bullet_line.get_rect().height() > row * 1.5,
            "the item wraps: {:?} row {row}",
            bullet_line.get_rect()
        );
        let baseline = f64::from(renderer.first_baseline_style(
            "x",
            None,
            &bullet_line.line_style(theme.text_scale),
        ));
        assert!(disc.center.y > top + row / 2.0 - 0.01, "scale {scale}");
        assert!(disc.center.y < top + baseline, "scale {scale}");

        let label = list_marker::ordinal_label(1, 0);
        let (origin, style) = list_marker::ordinal_placement(
            &editor.blocks[1].line,
            number_edge,
            &label,
            &theme,
            &mut renderer,
        );
        let (label_w, _) = renderer.measure_style(&label, None, &style);
        let number_right = origin.x + f64::from(label_w);
        let bullet_right = disc.center.x + disc.radius;
        let checkbox = list_marker::checklist_box(&editor.blocks[2].line, box_edge, scale.into());
        assert!((number_right - bullet_right).abs() < 0.01, "scale {scale}");
        assert!((checkbox.x1 - bullet_right).abs() < 0.01, "scale {scale}");
        assert!(
            (number_text - number_right - (bullet_text - bullet_right)).abs() < 0.01,
            "the text starts the same gap after every marker"
        );
        // The ordinal shares the item's first row.
        assert!((origin.y - editor.blocks[1].line.first_row_band().0).abs() < 0.01);
    }
}

#[test]
fn consecutive_list_items_sit_closer_than_paragraphs() {
    let theme = Theme::dark();
    let mut editor = page(vec![
        Block::paragraph("First paragraph"),
        Block::paragraph("Second paragraph"),
        Block::bullet(0, "One"),
        Block::bullet(0, "Two"),
        Block::checklist(1, false, "Nested task"),
        Block::numbered(0, "Three"),
        Block::paragraph("After the list"),
    ]);
    lay_out_and_draw(&mut editor, &theme);
    // From the bottom of one block's text to the top of the next's.
    let gap = |index: usize| {
        let (above_top, above_row) = editor.blocks[index - 1].line.first_row_band();
        let (below_top, _) = editor.blocks[index].line.first_row_band();
        below_top - (above_top + above_row)
    };
    let paragraphs = gap(1);
    for item in [3, 4, 5] {
        assert!(
            gap(item) < paragraphs,
            "list item {item}: {} vs paragraphs {paragraphs}",
            gap(item)
        );
        assert!(gap(item) >= 0.0);
    }
    assert!(
        gap(2) >= paragraphs,
        "a list stands off the prose before it"
    );
    assert!(gap(6) >= paragraphs, "and after it");
}

#[test]
fn nested_ordinals_follow_tex_enumerate_and_are_announced_as_shown() {
    let theme = Theme::dark();
    let mut editor = page(vec![
        Block::numbered(0, "Top"),
        Block::numbered(1, "Letter"),
        Block::numbered(2, "Roman"),
        Block::numbered(2, "Roman two"),
        Block::numbered(1, "Letter two"),
    ]);
    lay_out_and_draw(&mut editor, &theme);
    let ordinals = numbered_ordinals(&editor.blocks);
    let labels: Vec<String> = editor
        .blocks
        .iter()
        .zip(&ordinals)
        .map(|(block, ordinal)| list_marker::ordinal_label(*ordinal, block.indent()))
        .collect();
    assert_eq!(labels, ["1.", "(a)", "i.", "ii.", "(b)"]);
    let mut names = Vec::new();
    access_names(&Widget::accessibility(&editor).unwrap(), &mut names);
    for expected in [
        "Numbered item 1. block 1",
        "Numbered item (a) block 2",
        "Numbered item i. block 3",
        "Numbered item (b) block 5",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected}: {names:?}"
        );
    }
    // Markdown keeps the digits.
    assert_eq!(
        editor.to_markdown(),
        "1. Top\n\n  1. Letter\n\n    1. Roman\n\n    2. Roman two\n\n  2. Letter two\n"
    );
}

#[test]
fn a_paragraph_nested_under_an_item_starts_at_the_item_text() {
    let theme = Theme::dark();
    let mut editor = page(vec![
        Block::bullet(0, "Item"),
        Block::paragraph("More about the item"),
        Block::numbered(1, "Deeper"),
        Block::paragraph("More about the deeper item"),
    ]);
    // Tab on the prose below an item nests it under that item (its
    // outline children move with it).
    editor.focus_block(1, CursorPlacement::End);
    assert!(editor.try_indent(1, 1));
    assert_eq!(editor.blocks[1].indent(), 1);
    editor.focus_block(3, CursorPlacement::End);
    let under_deeper = editor.blocks[2].indent() + 1;
    while editor.blocks[3].indent() < under_deeper {
        assert!(editor.try_indent(3, 1));
    }
    lay_out_and_draw(&mut editor, &theme);
    let (item, _) = text_start_and_edge(&editor, 0);
    let (prose, _) = text_start_and_edge(&editor, 1);
    assert!((item - prose).abs() < 0.01, "{item} vs {prose}");
    let (deeper, _) = text_start_and_edge(&editor, 2);
    let (deeper_prose, _) = text_start_and_edge(&editor, 3);
    assert!(
        (deeper - deeper_prose).abs() < 0.01,
        "{deeper} vs {deeper_prose}"
    );
}

#[test]
fn a_callout_paints_its_wash_under_its_header_and_text() {
    let theme = Theme::dark();
    let mut editor = page(vec![
        Block::paragraph("Before"),
        Block::callout(CalloutKind::Warning, "Mind the gap"),
    ]);
    let (scene, _) = lay_out_and_draw(&mut editor, &theme);
    let colors = &theme.tokens.colors;
    let wash = colors.wash_on(
        colors.surfaces.content,
        crate::widget::Tone::Warning,
        crate::tokens::colors::WashStrength::Rest,
    );
    assert!(
        scene_fills(&scene, wash.fill),
        "the warning wash is painted"
    );
    assert!(
        scene_fills(&scene, wash.mark),
        "its icon and name in the status ink"
    );

    // The header band holds the icon and name above the text.
    let line = editor.blocks[1].line.get_rect();
    let painted = editor.block_paint_rect(1).unwrap();
    assert!(
        (line.y0 - painted.y0 - callout::callout_header_height()).abs() < 0.01,
        "{line:?} in {painted:?}"
    );
    let mut names = Vec::new();
    access_names(&Widget::accessibility(&editor).unwrap(), &mut names);
    assert!(
        names.iter().any(|name| name == "Warning callout block 2"),
        "{names:?}"
    );

    // Without a callout, no wash.
    let mut plain = page(vec![Block::paragraph("Mind the gap")]);
    let (scene, _) = lay_out_and_draw(&mut plain, &theme);
    assert!(!scene_fills(&scene, wash.fill));
}

#[test]
fn callouts_store_and_write_as_github_alerts() {
    for kind in CalloutKind::ALL {
        let block_kind = BlockKind::Callout(kind);
        assert_eq!(
            BlockKind::from_stored(&block_kind.stored()),
            Some(block_kind)
        );
        let markdown = format!("> {}\n> Mind\n> the gap\n", kind.marker());
        let editor = BlockEditor::from_markdown(&markdown);
        assert_eq!(editor.blocks.len(), 1);
        assert_eq!(editor.blocks[0].kind, BlockKind::Callout(kind));
        assert_eq!(editor.blocks[0].text(), "Mind\nthe gap");
        assert_eq!(editor.to_markdown(), markdown);
    }
    assert_eq!(BlockKind::from_stored("callout:danger"), None);
    assert_eq!(BlockKind::from_stored("quote"), Some(BlockKind::Quote));
}

#[test]
fn backspace_at_a_callouts_start_and_enter_in_an_empty_one_leave_it() {
    let mut editor = page(vec![Block::callout(CalloutKind::Tip, "")]);
    editor.focus_block(0, CursorPlacement::Start);
    editor.apply_signal(0, &LineSignal::Enter { split_at: 0 });
    assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(editor.blocks.len(), 1);

    let mut editor = page(vec![Block::callout(CalloutKind::Note, "Keep me")]);
    editor.focus_block(0, CursorPlacement::Start);
    editor.apply_signal(0, &LineSignal::BackspaceAtStart);
    assert_eq!(editor.blocks[0].kind, BlockKind::Paragraph);
    assert_eq!(editor.blocks[0].text(), "Keep me");
}
