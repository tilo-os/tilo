use super::*;
use crate::hosting::testing::TestHost;

type App = TestHost;

fn effect(app: &mut App, effect: InstallerPreviewEffect) {
    app.effect(&effect)
        .unwrap_or_else(|error| panic!("{} failed: {error:?}", effect.id()));
}

fn choose(app: &mut App, choice: &str) {
    effect(
        app,
        InstallerPreviewEffect::ChoosePreview {
            choice: choice.into(),
        },
    );
    effect(app, InstallerPreviewEffect::ContinuePreview);
}

fn state(app: &mut App) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}

fn preview() -> App {
    let mut app = App::launched(&["--setup"]);
    // Keep discovery out of navigation tests. Every screen is still reached
    // through the production action route, not a page fixture.
    app.with_widget::<InstallerPreview, _>(|view| {
        view.live = false;
        // The Linux route these tests walk; a Windows host takes the native
        // Full Tilo setup straight from its destination.
        view.full_os_host = false;
        view.storage.stage();
    })
    .unwrap();
    app
}

tilo_test_env::scenario! {
    fn back_returns_to_the_screen_the_person_came_from;

    phase install_back_keeps_the_normal_os_history;
    {
        let mut app = preview();
        choose(&mut app, "computer");
        assert_eq!(state(&mut app)["page"], "migrate");
        effect(&mut app, InstallerPreviewEffect::ContinuePreview);
        for next in [
            InstallerPreviewEffect::SetPreviewDeviceName {
                value: "Ada's computer".into(),
            },
            InstallerPreviewEffect::ContinuePreview,
        ] {
            effect(&mut app, next);
        }
        assert_eq!(state(&mut app)["page"], "storage");
        effect(&mut app, InstallerPreviewEffect::BackInPreview);
        assert_eq!(state(&mut app)["page"], "devices");
        effect(&mut app, InstallerPreviewEffect::BackInPreview);
        assert_eq!(state(&mut app)["page"], "migrate");
        effect(&mut app, InstallerPreviewEffect::BackInPreview);
        assert_eq!(state(&mut app)["page"], "destination");
    }
}

tilo_test_env::scenario! {
    #[cfg(target_os = "linux")]
    fn the_desktop_review_returns_to_its_destination;

    phase desktop_review_back_still_returns_directly_to_destination;
    {
        let mut app = preview();
        choose(&mut app, "desktop");
        assert_eq!(state(&mut app)["page"], "review");
        effect(&mut app, InstallerPreviewEffect::BackInPreview);
        assert_eq!(state(&mut app)["page"], "destination");
        assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
    }
}
