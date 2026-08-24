//! A pinned demo home is a boundary, not a redirection.
//!
//! The screenshot harness and the demo rigs all stage a home and point
//! `QOS_USER_HOME` at it, so that a frame of Files, a file chooser's
//! breadcrumb and a palette query show staged furniture instead of the
//! machine that happens to be running them. Frames had come back carrying
//! an operator's real project folders, a localized folder set from their
//! account, and — three letters into the palette — real private paths.
//!
//! The unit tests in `search::providers::files` prove that a walk cannot
//! leave the root it was given. This proves the other half, which is the
//! half that actually regressed: that setting the variable reaches the
//! production surfaces at all. Both of these read the environment through
//! their own resolution path, and each had a version that ignored the
//! override and read the process's own home instead.
//!
//! **One test per binary, deliberately.** It pins `QOS_USER_HOME` and
//! `HOME` for the whole process, which is only sound while nothing is
//! running beside it. A second test in this file reintroduces the race
//! that keeping it out of the ~1600-test lib binary is avoiding.

use shell::portal::picker_queue::PickerScope;
use shell::shell::search::providers::FilesProvider;

#[test]
fn a_pinned_home_confines_the_file_chooser_and_the_palette() {
    // Captured before the pin, because `dirs::home_dir()` reads `HOME`
    // and is about to start answering with the staged one.
    let real_home = dirs::home_dir().expect("the test machine has a home directory");

    let staged = tempfile::tempdir().expect("tempdir");
    let home = staged.path().to_path_buf();
    std::fs::create_dir_all(home.join("Documents")).expect("stage Documents");
    std::fs::write(home.join("Documents").join("Launch plan.md"), b"x").expect("stage a document");

    // SAFETY: this binary holds exactly one test, so no other thread is
    // reading the environment while it is written. See the module note.
    unsafe {
        std::env::set_var("QOS_USER_HOME", &home);
        std::env::set_var("HOME", &home);
    }

    // The file chooser. `roots` is the set the picker widget refuses to
    // navigate outside of, so it is the reach itself rather than a
    // presentation detail; `initial_dir` is what the breadcrumb draws,
    // component by component, in the opening frame.
    let scope = PickerScope::for_user();
    assert!(
        scope.initial_dir.starts_with(&home),
        "the file chooser opens on {}, outside the pinned home {} — its \
         breadcrumb draws those components on screen",
        scope.initial_dir.display(),
        home.display()
    );
    for root in &scope.roots {
        assert!(
            !root.starts_with(&real_home),
            "the file chooser can navigate to {}, which is inside the real \
             home {}. The pin is not reaching PickerScope::for_user.",
            root.display(),
            real_home.display()
        );
    }

    // The palette. Every path a query can return is discovered by walking
    // this root, so the root *is* the bound on what a keystroke can
    // surface. This provider previously read `dirs::home_dir()` and
    // ignored the override entirely.
    let provider = FilesProvider::new();
    assert_eq!(
        provider.root(),
        home.as_path(),
        "the palette is indexing {} rather than the pinned home {} — a \
         needle typed on camera can reach anything under it",
        provider.root().display(),
        home.display()
    );
    assert!(
        !provider.root().starts_with(&real_home),
        "the palette is indexing a directory inside the real home {}",
        real_home.display()
    );
}
