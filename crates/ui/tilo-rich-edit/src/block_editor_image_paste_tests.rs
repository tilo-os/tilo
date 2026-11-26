//! Ctrl+V with an image on the clipboard hands the image to the editor's host, which keeps its bytes
//! with the document; text still pastes as text.

use super::*;
use automerge::ROOT;
use tilo_ui_base::component::Described as _;
use ui::testing::Dispatch;
use winit::keyboard::ModifiersState;

fn ctrl_v(editor: &mut BlockEditor, doc: &mut AutoCommit) -> bool {
    let event = tilo_ui_base::synthetic_input::key_event(
        Key::Character("v".into()),
        None,
        ElementState::Pressed,
    );
    let handled = Dispatch::new()
        .modifiers(ModifiersState::CONTROL)
        .doc(doc)
        .send(
            editor,
            &tilo_ui_base::synthetic_input::keyboard_input(event),
        )
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
        tilo_ui_base::clipboard_copy(" more");
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
    fn kept(
        image: PictureIn<'_>,
    ) -> Result<ui::pictures::KeptPicture, ui::pictures::PictureRefusal> {
        Ok(match image {
            PictureIn::Bytes { name, bytes, .. } => ui::pictures::KeptPicture {
                name: name.to_owned(),
                address: format!("tilo-blob://{}", bytes.len()),
            },
            PictureIn::Path(path) => ui::pictures::KeptPicture {
                name: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                address: "tilo-blob://dropped".to_owned(),
            },
        })
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
        let texts: Vec<String> = editor
            .blocks()
            .iter()
            .map(|b| b.text().to_owned())
            .collect();
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
        let texts: Vec<String> = editor
            .blocks()
            .iter()
            .map(|b| b.text().to_owned())
            .collect();
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

/// A picture pasted into a document bound to a field is two changes on screen — the body's, and
/// the view naming the picture — and the editor names them one act of the person's, so the app's
/// naming action joins the same step and one undo takes both back.
#[test]
fn a_pasted_picture_names_itself_as_one_gesture_with_its_edit() {
    fn kept(_: PictureIn<'_>) -> Result<ui::pictures::KeptPicture, ui::pictures::PictureRefusal> {
        Ok(ui::pictures::KeptPicture {
            name: "Pasted image.png".to_owned(),
            address: "tilo-blob://pixels".to_owned(),
        })
    }
    let (services, recorder) = ui::process::ProcessServices::recording();
    let _host = ui::process::enter(&services);
    recorder.copy_image_elsewhere(screenshot());
    let queue = tilo_ui_base::Intents::<tilo_ui_base::component::Fired>::default();
    let mut editor = BlockEditor::from_props(
        "b",
        crate::BlockEditorProps {
            rich: tilo_ui_base::component::Bound {
                edits: Some(tilo_ui_base::component::Intent::new(
                    &queue, "b", "richtext",
                )),
                ..tilo_ui_base::component::Bound::default()
            },
            image: tilo_ui_base::component::Event::View(tilo_ui_base::component::Intent::new(
                &queue, "b", "image",
            )),
            ..crate::BlockEditorProps::default()
        },
    );
    editor.set_image_inserter(Some(Box::new(kept)));
    editor.focus_block(0, CursorPlacement::End);
    let mut doc = AutoCommit::new();
    assert!(ctrl_v(&mut editor, &mut doc));

    let fired = queue.take();
    let image = fired
        .iter()
        .find(|fired| fired.event == "image")
        .expect("the view hears the picture's name and where its bytes are");
    assert_eq!(
        image.value,
        Some(tilo_ui_base::component::Value::List(vec![
            tilo_ui_base::component::Value::Text("Pasted image.png".to_owned()),
            tilo_ui_base::component::Value::Text("tilo-blob://pixels".to_owned()),
        ]))
    );
    let gesture = image.gesture.expect("the naming is one act with the edit");
    let edit = fired
        .iter()
        .find(|fired| fired.event == "richtext")
        .expect("the view hears the body's change");
    assert_eq!(
        edit.gesture,
        Some(gesture),
        "the body's change is the same act as the naming"
    );
}
