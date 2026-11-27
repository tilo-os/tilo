use super::{Early, early_for};

tilo_test_env::scenario! {
    fn an_early_window_or_surface_is_kept_only_for_its_owner;

    phase an_early_window_is_kept_only_for_the_app_id_it_announces;
    {
        let made = Some(("so.tilo.notes".to_string(), "window"));
        assert_eq!(
            early_for(made.clone(), &"so.tilo.notes".to_string()),
            Early::Kept("window")
        );
        // An input surface announces its own id, never the package's: the
        // early window must not become its window.
        assert_eq!(
            early_for(made, &"so.tilo.keyboard".to_string()),
            Early::Discarded
        );
        assert_eq!(
            early_for(None::<(String, &str)>, &"so.tilo.notes".to_string()),
            Early::Absent
        );
    }

    phase a_prepared_surface_is_used_only_by_the_window_it_was_made_for;
    {
        assert_eq!(
            early_for(Some((7_u64, "surface")), &7),
            Early::Kept("surface")
        );
        assert_eq!(early_for(Some((7_u64, "surface")), &8), Early::Discarded);
    }
}
