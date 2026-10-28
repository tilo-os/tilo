use std::path::Path;

use crate::app::EditorDoc;

fn file_parent(file: &Path) -> std::path::PathBuf {
    file.parent().unwrap().to_path_buf()
}

/// The active buffer's file, its stamp and whether it has unsaved edits.
fn shown(app: &super::CodeApp) -> Option<(Option<std::path::PathBuf>, bool, bool)> {
    let buffer = app.session.active_buffer()?;
    Some((buffer.path.clone(), buffer.stamp.is_some(), buffer.dirty))
}

/// On the compositor's frame loop the editor reads, writes and lists the
/// disk on a background thread: opening a file (and reading the folders on
/// the way to it) and saving answer "not yet" there, and the outcome lands
/// on a later settlement, as it does at once anywhere else.
#[test]
fn the_frame_loop_opens_reads_and_saves_files_off_its_thread() {
    let mut sample = crate::test_support::sample();
    let file = crate::disk::canonical(&sample.root.join("src/main.rs"));
    let _loop = ui::storage::waiting::forbid_commit_waits_strictly();

    let opening = file.clone();
    let answer = sample.editor(move |app| app.open_disk_file(&opening));
    assert!(answer.is_none(), "the loop does not read the file itself");
    let wanted = file.clone();
    tilo_test_env::wait_until("the file to open and be read", || {
        sample.app.settle();
        sample.editor(|app| shown(app)) == Some((Some(wanted.clone()), true, false))
    });

    // The folders on the way to it were read off the loop too.
    let folder = file_parent(&file);
    let src = sample.editor(move |app| {
        let node = app.session.project.node_for_path(&folder).unwrap();
        app.session.project.folder_to_read(node)
    });
    assert_eq!(src, None, "src/ was read for the file");

    // A save on the loop lands later, and the tab's unsaved mark clears then.
    let landed = sample.with_doc(|app, doc| {
        app.editor.insert_for_test(doc, "// typed on the loop\n");
        app.dispatch_editor_text_edits(doc);
        let node = app.session.active_buffer().unwrap().node;
        app.save_buffer(doc, node, false)
    });
    assert!(!landed, "the loop does not write the file itself");
    tilo_test_env::wait_until("the save to land", || {
        sample.app.settle();
        sample
            .editor(|app| shown(app))
            .is_some_and(|(_, _, dirty)| !dirty)
    });
    let written = std::fs::read_to_string(&file).unwrap();
    let text = sample.with_doc(|app, doc| {
        EditorDoc::read_buffer_key(
            doc,
            &app.buffer_key(app.session.active_buffer().unwrap().node),
        )
    });
    assert_eq!(Some(written), text, "the file holds what the tab shows");
}
