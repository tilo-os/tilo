//! A namespace saved before the tree was kept in chunks (beca68fe58c) is one Automerge document in
//! a layout this build does not read. It is recognised as such, so opening the mount sets it aside
//! instead of refusing (`open_namespace` in the fs binary), while a current save is not mistaken
//! for it.

use super::*;

tilo_test_env::scenario! {
    fn a_namespace_saved_before_chunks_is_recognised;

    phase the_pre_chunk_code_s_save_is_recognised;
    {
        // Written by `Namespace::save` at beca68fe58c^ for a tree of `notes/plan.md`.
        assert!(Namespace::is_pre_chunk_save(&pre_chunk_fixture()));
    }

    phase a_current_save_is_not;
    {
        let mut current = Namespace::new("a").unwrap();
        current.create_dir(ROOT_INO, "notes", 1).unwrap();
        assert!(!Namespace::is_pre_chunk_save(&current.save()));
        assert!(!Namespace::is_pre_chunk_save(b"not a namespace"));
    }
}
