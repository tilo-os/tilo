//! What a launch names: Files launched on a folder shows it, and on a file
//! ("Show in Files", a screenshot's receipt) shows its folder with the file
//! selected.

use super::*;

#[test]
fn a_launch_naming_a_file_shows_it_selected_in_its_folder() {
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join("Screenshots");
    std::fs::create_dir(&folder).unwrap();
    for name in ["a.png", "Tilo Screenshot.png", "z.png"] {
        std::fs::write(folder.join(name), b"").unwrap();
    }
    let files = FileExplorerWidget::with_persistence(None, None);
    files.state().navigate(home.path().to_path_buf());

    files
        .state()
        .open_launch_argument(&folder.join("Tilo Screenshot.png").to_string_lossy());
    assert_eq!(
        files.state().current_path,
        folder,
        "the file's folder is shown"
    );
    let selected = {
        let state = files.state();
        state
            .selection
            .primary()
            .map(|index| state.displayed_entries()[index].name.clone())
    };
    assert_eq!(selected.as_deref(), Some("Tilo Screenshot.png"));

    files
        .state()
        .open_launch_argument(&home.path().to_string_lossy());
    assert_eq!(
        files.state().current_path,
        home.path(),
        "a folder is shown itself"
    );
}
