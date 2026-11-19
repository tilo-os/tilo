//! Every app of the fleet answers the surface it publishes, so a fresh
//! workspace's generated SDK covers every installed app.

use super::tests::{Notes, agent};

tilo_test_env::scenario! {
    fn every_installed_app_answers_what_it_publishes;

    phase a_fleet_app_answers_its_surface_without_a_window_or_a_package;
    // What `tilo sdk gen -w` reads for each installed app on a fresh
    // workspace: the app's surface, with the app neither started nor its
    // package kept here.
    {
        let notes = Notes::new(false);
        let server = agent(&notes);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for app in app_fleet::apps() {
                let surface = server
                    .surface_bytes(app.id)
                    .await
                    .unwrap_or_else(|| panic!("{} answers no surface", app.id));
                let (_, kept) = collab_types::surface::SurfaceFile::kept(
                    app_fleet::surface(app.id)
                        .expect("the fleet compiles its surface in")
                        .as_bytes(),
                )
                .expect("the fleet's surface is one");
                assert_eq!(
                    String::from_utf8_lossy(&surface),
                    kept,
                    "{}",
                    app.id
                );
            }
        });
    }
}
