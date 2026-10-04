//! Themes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the theme library, and the effects that reach outside it.

use automerge::ROOT;
use ui::framework::{ResourceApp, SeedCx, ViewCx};
use ui::prelude::{Effect, tilo_resource};
use ui::tilo_actions::Resource as PublishedResource;
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    THEMES_ACTION_TYPE, ThemesAction, document_children, set_active_theme_action,
    upsert_custom_theme_action,
};
use crate::app::ThemesApp;
use crate::doc::{self, KEY_ACTIVE};
use crate::model::{ThemeSpec, default_catalog, demo_catalog};
use crate::store;

/// Catalog plus the active theme id.
#[derive(Clone, Debug, Default)]
pub struct ThemesState {
    pub catalog: Vec<ThemeSpec>,
    pub active: Option<String>,
}

/// The themes library resource.
pub struct Themes;

impl Resource for Themes {
    const KIND: &'static str = "themes.library";
    const LABEL: &'static str = "Themes";
    const ACTION_TYPE: &'static str = THEMES_ACTION_TYPE;

    type State = ThemesState;
    type Action = ThemesAction;

    /// The custom-theme map and its tombstones exist from the genesis, so
    /// two devices that each add a theme before they sync add them to one
    /// map rather than to two competing ones.
    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc::initialize_catalog(doc, &ROOT);
        Ok(())
    }

    fn read(doc: &Doc) -> Result<ThemesState, ModelError> {
        Ok(ThemesState {
            catalog: doc::load_catalog(doc, &ROOT).unwrap_or_default(),
            active: ui::doc::read_str(doc, &ROOT, KEY_ACTIVE),
        })
    }

    /// A slider, the colour picker or the name field edits one field of one
    /// theme a value at a time; every value of one gesture is one step.
    fn coalesces(previous: &ThemesAction, next: &ThemesAction) -> bool {
        next.continues(previous)
    }

    fn children(state: &ThemesState) -> Vec<PublishedResource> {
        document_children(state)
    }

    fn search(state: &ThemesState) -> Vec<SearchEntry> {
        state
            .catalog
            .iter()
            .map(|theme| SearchEntry {
                anchor: Some(format!("themes.theme:{}", theme.id)),
                title: theme.name.clone(),
                text: theme.id.clone(),
                action: None,
            })
            .collect()
    }
}

/// What the studio does beyond its document: create a theme it then shows,
/// make a theme the system's look, and hand a theme out as JSON.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "themes.effect")]
pub enum ThemesEffect {
    /// New theme
    New {
        #[param("Name of the new theme")]
        name: Option<String>,
    },
    /// Apply as system theme
    #[effect(job, cancellable)]
    ApplyTheme {
        #[param("Id of the theme to apply; the selected one when left out")]
        theme: Option<String>,
    },
    /// Export theme JSON
    #[effect(returns, permission = "read")]
    Export {
        #[param("Id of the theme to export; the selected one when left out")]
        theme: Option<String>,
    },
}

/// Screenshot scenarios. Every one but [`LIVE`] shows the library with one
/// custom theme beside the presets.
pub mod scenario {
    /// A preset on a phone-sized window: its full specimen, scrolled.
    pub const LIVE: &str = "live";
    /// The custom theme's full specimen on a phone-sized window, scrolled.
    pub const PREVIEW: &str = "preview";
    /// The custom theme's controls on a phone-sized window.
    pub const CUSTOMIZE: &str = "customize";
    /// The library's actions menu on the custom theme's row.
    pub const MENU: &str = "menu";
    /// The custom theme's controls, scrolled partway.
    pub const SCROLL: &str = "scroll";
    /// A preset whose specimen is set in a monospace font.
    pub const FONT: &str = "font";
    /// The library sliding over a phone-sized window.
    pub const GALLERY: &str = "gallery";

    /// Every scenario.
    pub const ALL: &[&str] = &[LIVE, PREVIEW, CUSTOMIZE, MENU, SCROLL, FONT, GALLERY];

    /// Whether `name` is captured on a phone-sized window.
    pub fn narrow(name: Option<&str>) -> bool {
        matches!(
            name,
            Some(LIVE | PREVIEW | CUSTOMIZE | MENU | SCROLL | GALLERY)
        )
    }
}

/// Framework app: the themes widget is the surface over [`Themes`].
pub struct ThemesShell;

impl ResourceApp for ThemesShell {
    type Resource = Themes;
    const ID: &'static str = "so.tilo.themes";
    const TITLE: &'static str = "Themes";
    const ICON: Option<&'static str> = Some("themes");

    fn view(cx: &mut ViewCx<Themes>) -> Element {
        cx.surface(ThemesApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        scenario::ALL
    }

    fn shot_viewport(name: Option<&str>) -> (u32, u32) {
        if scenario::narrow(name) {
            (480, 900)
        } else {
            (1280, 800)
        }
    }

    /// The custom themes the library shows beside the presets.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Themes>> {
        if name == Some(scenario::LIVE) {
            return Vec::new();
        }
        demo_catalog()
            .iter()
            .filter(|spec| !spec.builtin)
            .filter_map(|spec| Invoke::apply(&upsert_custom_theme_action(spec)).ok())
            .collect()
    }

    /// The theme this device already shows: the one applied here before
    /// (imported with its definition when it is a custom theme), else the
    /// first preset.
    fn seed(cx: &SeedCx<'_>) -> Vec<Invoke<Themes>> {
        let applied = store::applied(cx.host().storage().settings_document());
        let mut actions = Vec::new();
        let active = match applied {
            Some(spec) if !spec.builtin => {
                actions.push(upsert_custom_theme_action(&spec));
                Some(spec.id)
            }
            Some(spec) => Some(spec.id),
            None => default_catalog().first().map(|spec| spec.id.clone()),
        };
        actions.extend(active.map(set_active_theme_action));
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ThemesAction, set_custom_theme_radius_action};
    use crate::model::TokenId;
    use ui::prelude::TestApp;

    #[test]
    fn edits_of_one_field_continue_one_another_and_nothing_else_does() {
        let radius = |id: &str, value| set_custom_theme_radius_action(id, value);
        assert!(Themes::coalesces(&radius("a", 0.1), &radius("a", 0.2)));
        assert!(!Themes::coalesces(&radius("a", 0.1), &radius("b", 0.2)));
        assert!(!Themes::coalesces(
            &radius("a", 0.1),
            &ThemesAction::SetCustomThemeSpacing {
                id: "a".into(),
                spacing_norm: 0.2
            }
        ));
        let token = |token| ThemesAction::SetCustomThemeToken {
            id: "a".into(),
            token,
            rgb: [1, 2, 3],
        };
        assert!(Themes::coalesces(
            &token(TokenId::Accent),
            &token(TokenId::Accent)
        ));
        assert!(!Themes::coalesces(
            &token(TokenId::Accent),
            &token(TokenId::Border)
        ));
        let delete = ThemesAction::DeleteCustomTheme { id: "a".into() };
        assert!(!Themes::coalesces(&delete, &delete));
    }

    /// A new library shows the theme this device applied, and the seed that
    /// put it there is nobody's undo step.
    #[test]
    fn a_new_library_starts_on_the_theme_this_device_applied() {
        let mut fresh = TestApp::<ThemesShell>::new().unwrap();
        assert_eq!(
            fresh.state().active.as_deref(),
            default_catalog().first().map(|theme| theme.id.as_str())
        );
        assert!(!fresh.undo(), "the seed is not an undo step");

        // A custom theme applied on this device comes along with its
        // definition.
        let profile = tempfile::tempdir().unwrap();
        let mut sunset = crate::model::seed_custom();
        sunset.name = "Dusk".into();
        store::save(
            &ui::storage::SettingsDocument::File(profile.path().join("settings.json")),
            &sunset,
        )
        .unwrap();
        let imported = TestApp::<ThemesShell>::open_data_dir(profile.path()).unwrap();
        let state = imported.state();
        assert_eq!(state.active.as_deref(), Some("sunset"));
        assert!(
            state
                .catalog
                .iter()
                .any(|theme| theme.id == "sunset" && theme.name == "Dusk" && !theme.builtin)
        );
    }

    /// The library with the custom theme "sunset" beside the presets, in a
    /// window someone uses, on a profile whose settings file the test reads.
    fn demo() -> (TestApp<ThemesShell>, tempfile::TempDir) {
        let profile = tempfile::tempdir().unwrap();
        let mut app = TestApp::<ThemesShell>::open_data_dir(profile.path()).unwrap();
        for invoke in ThemesShell::scenario(None) {
            app.invoke(invoke).unwrap();
        }
        (app, profile)
    }

    #[test]
    fn effects_create_export_and_apply_themes() {
        let (mut app, profile) = demo();
        let before = app.state().catalog.len();

        app.effect(&ThemesEffect::New {
            name: Some("Ocean".into()),
        })
        .unwrap();
        app.settle();
        let catalog = app.state().catalog;
        assert_eq!(catalog.len(), before + 1);
        assert!(
            catalog
                .iter()
                .any(|theme| theme.name == "Ocean" && !theme.builtin)
        );

        // Export hands the theme out and leaves the library as it is.
        let heads = app.read_document(|doc| doc.clone().get_heads());
        let exported = app
            .effect(&ThemesEffect::Export {
                theme: Some("sunset".into()),
            })
            .unwrap();
        let exported: ThemeSpec =
            serde_json::from_value(exported.value.expect("the exported theme")).unwrap();
        assert_eq!(exported.id, "sunset");
        assert_eq!(app.read_document(|doc| doc.clone().get_heads()), heads);

        // Apply makes the theme the device's look and the library's active one.
        app.effect(&ThemesEffect::ApplyTheme {
            theme: Some("dracula".into()),
        })
        .unwrap();
        app.settle();
        assert_eq!(app.state().active.as_deref(), Some("dracula"));
        let settings = ui::storage::SettingsDocument::File(profile.path().join("settings.json"));
        assert_eq!(store::load_active_id(&settings).as_deref(), Some("dracula"));

        // A theme that is not in the library is refused.
        assert!(
            app.effect(&ThemesEffect::ApplyTheme {
                theme: Some("missing".into()),
            })
            .is_err()
        );
    }

    /// Every screenshot scenario stages and settles, at its viewport, and
    /// the menu scenario shows the custom theme's actions.
    #[test]
    fn every_scenario_captures() {
        for name in scenario::ALL {
            let mut app = TestApp::<ThemesShell>::capture(Some(name)).unwrap();
            let (width, height) = ThemesShell::shot_viewport(Some(name));
            assert_eq!(app.harness().inspection().viewport, [width, height]);
            let custom = app.state().catalog.iter().any(|theme| !theme.builtin);
            assert_eq!(custom, *name != scenario::LIVE, "{name}");
        }
        let mut menu = TestApp::<ThemesShell>::capture(Some(scenario::MENU)).unwrap();
        assert_eq!(
            menu.surface(|app: &mut ThemesApp| app.gallery_menu_is_open()),
            Some(true)
        );
    }
}
