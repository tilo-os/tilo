//! German, chosen with the pack installed as a session installs it.

use settings::SettingsShell;
use settings::panes::region::language::LanguageEffect;
use settings::prefs::AppearancePrefs;
use ui::framework::testing::TestApp;

/// The pack a session installs is named `de`; its catalogues are tagged `de-DE`.
#[test]
fn choosing_the_installed_german_pack_is_taken_by_any_tag_of_it() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let pack = home.data().join("tilo/language-packs/de");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(
        pack.join("settings.ftl"),
        "settings-title = Einstellungen\n",
    )
    .unwrap();
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let choose = |app: &mut TestApp<SettingsShell>, language: &str| {
        app.effect(&LanguageEffect::ChooseLanguage {
            language: language.to_owned(),
        })
    };
    for tag in ["de-DE", "de_DE", "de"] {
        choose(&mut app, tag).unwrap_or_else(|error| panic!("choosing {tag}: {error}"));
        app.settle();
        assert_eq!(
            app.settings::<AppearancePrefs>().get().language.as_deref(),
            Some("de"),
            "the desktop follows the pack after choosing {tag}"
        );
    }
}
