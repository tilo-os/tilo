//! Ctrl+V with an image on the clipboard hands the image to the editor's
//! host, which keeps its bytes with the document; text still pastes as text.

use super::*;
use automerge::ROOT;
use winit::keyboard::ModifiersState;

fn ctrl_v(editor: &mut BlockEditor, doc: &mut AutoCommit) -> bool {
    let event =
        crate::synthetic_input::key_event(Key::Character("v".into()), None, ElementState::Pressed);
    let mut handled = false;
    editor.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        },
        &mut crate::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::from(ModifiersState::CONTROL),
            state: crate::widget::Edits::lend(doc),
            path: &ROOT,
            handled: &mut handled,
            focus_target: &mut None,
            node_id: NodeId::new(0),
            taffy: &TaffyTree::new(),
        },
    );
    editor.sync(doc, &ROOT);
    handled
}

fn screenshot() -> crate::clipboard::ClipboardImage {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    crate::clipboard::ClipboardImage {
        mime: "image/png",
        bytes: bytes.into_inner(),
    }
}

#[test]
fn ctrl_v_hands_a_copied_image_to_the_host() {
    fn in_prose_the_image_goes_to_the_host_and_the_text_stays() {
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("caption")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        assert!(ctrl_v(&mut editor, &mut doc));
        assert_eq!(editor.take_pasted_images(), vec![screenshot()]);
        assert!(editor.take_pasted_images().is_empty(), "taken once");
        assert_eq!(editor.blocks()[0].text(), "caption");
    }

    fn a_semantic_paste_does_the_same() {
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("caption")]);
        editor.focus_block(0, CursorPlacement::End);
        assert!(editor.edit_from_semantics("paste"));
        assert_eq!(editor.take_pasted_images().len(), 1);
    }

    fn code_takes_text_not_images() {
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::code_block(None, "x = 1")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        let _ = ctrl_v(&mut editor, &mut doc);
        assert!(editor.take_pasted_images().is_empty());
    }

    fn copied_text_pastes_as_text() {
        let (services, recorder) = crate::host::HostServices::recording();
        let _host = crate::host::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        crate::clipboard_copy(" more");
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("caption")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        let _ = ctrl_v(&mut editor, &mut doc);
        assert!(editor.take_pasted_images().is_empty());
    }

    in_prose_the_image_goes_to_the_host_and_the_text_stays();
    a_semantic_paste_does_the_same();
    code_takes_text_not_images();
    copied_text_pastes_as_text();
}
