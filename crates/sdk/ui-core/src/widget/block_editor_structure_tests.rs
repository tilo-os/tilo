//! Another document's stack, laid out in the editor that stands: no change above it.

use super::*;

fn laid_out_editor(
    blocks: Vec<Block>,
) -> (
    BlockEditor,
    TaffyTree<()>,
    HashMap<NodeId, *mut dyn Widget>,
    NodeId,
) {
    let mut editor = BlockEditor::new().with_wrap_lines(true);
    editor.set_blocks(blocks);
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = editor.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
    // The build places what the editor holds; it owes the host nothing for it.
    let _ = editor.take_structure_change();
    (editor, taffy, nodes, node)
}

/// Another document is another block stack in the same editor: the stack is laid out again
/// where it stands, and a picture that comes or goes is a control the editor mounts and lets
/// go of itself. Nothing above the editor is replaced, so a window that shows another note —
/// or another task — keeps its composition.
#[test]
fn another_document_changes_nothing_above_the_editor() {
    let theme = Theme::dark();
    let picture = Block::paragraph("![An attachment](fixture.png)").content();
    let (mut editor, mut taffy, mut nodes, node) = laid_out_editor(vec![
        Block::paragraph("first"),
        Block::from_content(picture.clone()),
    ]);
    assert_eq!(
        taffy
            .children(node)
            .expect("the editor's layout node")
            .len(),
        2,
        "the block stack and the picture's control stand under the editor"
    );

    // A document that keeps the picture: the stack changed, the control under it did not.
    editor.set_blocks(vec![
        Block::paragraph("second"),
        Block::from_content(picture.clone()),
    ]);
    assert_eq!(
        editor.take_structure_change(),
        StructureChange::None,
        "another document's stack is no structure change above the editor"
    );
    assert!(
        editor.is_layout_dirty(),
        "the stack is laid out again in place"
    );

    // A document the person took the picture out of: the control is the editor's to let go of.
    editor.set_blocks(vec![Block::paragraph("no picture here")]);
    assert_eq!(
        editor.take_structure_change(),
        StructureChange::Children,
        "the picture's control is a change of the editor's own children, never a rebuild"
    );
    let mut remount = Remount::new(&mut taffy, &theme, &mut nodes);
    assert!(
        editor.remount_children(&mut remount),
        "the editor carries its own change out in place"
    );
    assert_eq!(
        taffy
            .children(node)
            .expect("the editor's layout node")
            .len(),
        1,
        "the stack's leaf stands alone once the picture's control is gone"
    );
    assert_eq!(
        editor.take_structure_change(),
        StructureChange::None,
        "a change carried out in place is no longer the host's to carry"
    );

    // And a picture the other document brings: the control is mounted under the editor.
    editor.set_blocks(vec![
        Block::paragraph("a picture again"),
        Block::from_content(picture),
    ]);
    assert_eq!(editor.take_structure_change(), StructureChange::Children);
    let mut remount = Remount::new(&mut taffy, &theme, &mut nodes);
    assert!(editor.remount_children(&mut remount));
    assert_eq!(
        taffy
            .children(node)
            .expect("the editor's layout node")
            .len(),
        2,
        "the picture's control is mounted where the editor stands"
    );
    assert_eq!(editor.take_structure_change(), StructureChange::None);
}

/// A host places the editor itself in the page it shows: absolute, under the title pinned above
/// it, with the insets that page leaves. Carrying out a change under the editor in place leaves
/// that placement as the host set it: an editor stacked at the page's origin draws its first
/// blocks under the pinned title, and a press on a block reaches the title instead of the caret.
#[test]
fn a_remount_leaves_the_place_the_host_gave_the_editor() {
    let theme = Theme::dark();
    let (mut editor, mut taffy, mut nodes, node) = laid_out_editor(vec![
        Block::paragraph("first"),
        Block::from_content(Block::paragraph("![An attachment](fixture.png)").content()),
    ]);
    let placed = Style {
        position: taffy::Position::Absolute,
        size: Size {
            width: Dimension::auto(),
            height: Dimension::auto(),
        },
        inset: taffy::Rect {
            left: taffy::style_helpers::length(16.0_f32),
            right: taffy::style_helpers::length(24.0_f32),
            top: taffy::style_helpers::length(72.0_f32),
            bottom: taffy::LengthPercentageAuto::auto(),
        },
        ..taffy.style(node).expect("the editor's layout node").clone()
    };
    let _ = taffy.set_style(node, placed.clone());

    // A document without the picture: a change of the editor's own children, carried out here.
    editor.set_blocks(vec![Block::paragraph("no picture here")]);
    assert_eq!(editor.take_structure_change(), StructureChange::Children);
    let mut remount = Remount::new(&mut taffy, &theme, &mut nodes);
    assert!(editor.remount_children(&mut remount));

    let after = taffy.style(node).expect("the editor's layout node").clone();
    assert_eq!(
        after.position,
        taffy::Position::Absolute,
        "the editor stands where the host placed it"
    );
    assert_eq!(
        after.inset, placed.inset,
        "the page's insets are the host's"
    );
    assert_eq!(after.size, placed.size, "the host sizes the editor");
    assert_eq!(
        after.padding.left,
        taffy::style_helpers::length(editor.handle_column() as f32),
        "the editor keeps the leading column its drag handles stand in"
    );
}
