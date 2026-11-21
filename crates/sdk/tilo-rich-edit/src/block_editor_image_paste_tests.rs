//! Ctrl+V with an image on the clipboard hands the image to the editor's host, which keeps its bytes
//! with the document; text still pastes as text.

use super::*;
use automerge::ROOT;
use ui::testing::Dispatch;
use winit::keyboard::ModifiersState;

fn ctrl_v(editor: &mut BlockEditor, doc: &mut AutoCommit) -> bool {
    let event =
        ui::synthetic_input::key_event(Key::Character("v".into()), None, ElementState::Pressed);
    let handled = Dispatch::new()
        .modifiers(ModifiersState::CONTROL)
        .doc(doc)
        .send(editor, &ui::synthetic_input::keyboard_input(event))
        .handled;
    editor.sync(doc, &ROOT);
    handled
}

fn screenshot() -> ui::clipboard::ClipboardImage {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    ui::clipboard::ClipboardImage {
        mime: "image/png",
        bytes: bytes.into_inner(),
    }
}

#[test]
fn ctrl_v_hands_a_copied_image_to_the_host() {
    fn in_prose_the_image_goes_to_the_host_and_the_text_stays() {
        let (services, recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
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
        let (services, recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("caption")]);
        editor.focus_block(0, CursorPlacement::End);
        assert!(editor.edit_from_semantics("paste"));
        assert_eq!(editor.take_pasted_images().len(), 1);
    }

    fn code_takes_text_not_images() {
        let (services, recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::code_block(None, "x = 1")]);
        editor.focus_block(0, CursorPlacement::End);
        let mut doc = AutoCommit::new();
        let _ = ctrl_v(&mut editor, &mut doc);
        assert!(editor.take_pasted_images().is_empty());
    }

    fn copied_text_pastes_as_text() {
        let (services, recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        ui::clipboard_copy(" more");
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

#[test]
fn a_host_that_keeps_pictures_gets_the_picture_at_the_caret() {
    fn kept(image: PictureIn<'_>) -> Option<ui::pictures::KeptPicture> {
        match image {
            PictureIn::Bytes { name, bytes, .. } => Some(ui::pictures::KeptPicture {
                name: name.to_owned(),
                address: format!("tilo-blob://{}", bytes.len()),
            }),
            PictureIn::Path(path) => Some(ui::pictures::KeptPicture {
                name: path.file_name()?.to_str()?.to_owned(),
                address: "tilo-blob://dropped".to_owned(),
            }),
        }
    }

    fn the_picture_goes_in_at_the_caret_as_one_edit() {
        let (services, recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        recorder.copy_image_elsewhere(screenshot());
        let picture = screenshot();
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("caption")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.set_image_inserter(Some(Box::new(kept)));
        let mut doc = AutoCommit::new();
        assert!(ctrl_v(&mut editor, &mut doc));
        let texts: Vec<String> = editor.blocks().iter().map(|b| b.text().to_owned()).collect();
        assert_eq!(
            texts,
            vec![
                "caption".to_owned(),
                format!("![](tilo-blob://{})", picture.bytes.len())
            ],
            "the picture takes a line of its own under the words it was pasted after"
        );
        assert!(
            !editor.take_edit_ops().is_empty(),
            "the host hears of the picture as the document's edit"
        );
        assert!(
            editor.take_pasted_images().is_empty(),
            "the host that kept it has no request to place"
        );
    }

    fn a_picture_dropped_on_the_editor_goes_in_the_same_way() {
        let (services, _recorder) = ui::process::ProcessServices::recording();
        let _host = ui::process::enter(&services);
        let mut editor = BlockEditor::new();
        editor.set_blocks(vec![Block::paragraph("caption")]);
        editor.focus_block(0, CursorPlacement::End);
        editor.set_image_inserter(Some(Box::new(kept)));
        let doc = AutoCommit::new();
        let handled = ui::testing::dispatch(
            &mut editor,
            &winit::event::WindowEvent::DroppedFile("/tmp/fox.png".into()),
        );
        editor.sync(&doc, &ROOT);
        assert!(handled, "the editor took the drop the keyboard was in");
        let texts: Vec<String> = editor.blocks().iter().map(|b| b.text().to_owned()).collect();
        assert_eq!(
            texts,
            vec!["caption".to_owned(), "![](tilo-blob://dropped)".to_owned()],
            "a dropped picture takes a line of its own too"
        );
        assert!(!editor.take_edit_ops().is_empty());
    }

    the_picture_goes_in_at_the_caret_as_one_edit();
    a_picture_dropped_on_the_editor_goes_in_the_same_way();
}
