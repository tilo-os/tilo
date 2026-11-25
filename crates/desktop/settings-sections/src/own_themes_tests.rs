//! A person's own themes, as records of their appearance: made, edited role by role,
//! merged between two people, and refused when they would be illegible, through the same
//! actions and edits Settings, the shell and an agent use.
use ui::tilo_resource::AppNamed;

use super::AppearancePrefs;
use ui::theme::{Preset, PresetDesign as _, ThemeDocument, ThemeRecord};
use ui::tilo_resource::{Edit, Id, Invoke, Replica};

fn appearance_with(base: &str) -> (Replica<AppearancePrefs, AppNamed>, Id) {
    let mut appearance = Replica::<AppearancePrefs>::new().in_app("so.tilo.model", "Model");
    appearance
        .apply(AppearancePrefs::NEW_THEME.with(("mine".to_owned(), base.to_owned())))
        .expect("a preset is a base");
    (appearance, Id::from_raw("mine"))
}

fn edit(
    appearance: &mut Replica<AppearancePrefs, AppNamed>,
    edit: &Edit<AppearancePrefs>,
) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    appearance
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn theme(appearance: &Replica<AppearancePrefs, AppNamed>, id: &Id) -> ThemeRecord {
    appearance.state().unwrap().themes[id].clone()
}

#[test]
fn a_theme_round_trips_its_document_role_by_role() {
    for preset in Preset::ALL {
        let document = preset.document();
        let theme = ThemeRecord::from_document(&document);
        assert_eq!(theme.document(preset.id()), document);
        assert_eq!(theme.findings(preset.id()), Vec::<String>::new());
    }
}

#[test]
fn editing_a_finished_theme_keeps_its_look() {
    let document: ThemeDocument = serde_json::from_str(include_str!(
        "../../../ui/ui-core/tests/fixtures/finishes/liquid-glass.json"
    ))
    .unwrap();
    let mut theme = ThemeRecord::from_document(&document);
    theme.tint_opacity = 0.2;
    theme.density = 1.1;
    let edited = theme.document("mine");
    let floating = &edited.material.floating;
    assert_eq!(floating.painter(), document.material.floating.painter());
    assert!((floating.tint() - 0.2).abs() < 1e-6);
    assert_eq!(
        floating.param("refraction"),
        document.material.floating.param("refraction")
    );
    assert_eq!(edited.material.window, document.material.window);
    assert_eq!(edited.depth, document.depth);
    assert_eq!(edited.shape.border, document.shape.border);
}

#[test]
fn edits_stay_inside_the_admitted_bands_and_the_contrast_floors() {
    let (mut appearance, id) = appearance_with("dusk-dark");
    assert_eq!(theme(&appearance, &id).name, "Dusk dark copy");
    let paths = AppearancePrefs::paths().themes.item(&id);
    edit(&mut appearance, &paths.hue.set(Some(40.0))).unwrap();
    assert_eq!(theme(&appearance, &id).hue, Some(40.0));
    assert!(
        edit(&mut appearance, &paths.chroma.set(0.2)).is_err(),
        "too much colour"
    );
    assert!(edit(&mut appearance, &paths.window_radius.set(90.0)).is_err());
    assert!(edit(&mut appearance, &paths.density.set(f64::NAN)).is_err());
    let refused = edit(&mut appearance, &paths.ink.tertiary.set(0.3))
        .expect_err("ink may not sink into the surfaces");
    assert!(!refused.is_empty());
    edit(&mut appearance, &paths.surfaces.content.set(0.24)).unwrap();
    assert!((theme(&appearance, &id).surfaces.content - 0.24).abs() < 1e-6);
    assert!(
        edit(&mut appearance, &paths.dark.set(false)).is_err(),
        "polarity is switched, never set alone"
    );
    let nobody = AppearancePrefs::paths()
        .themes
        .item(&Id::from_raw("nobody"));
    assert!(edit(&mut appearance, &nobody.name.replace("x")).is_err());
}

#[test]
fn reviewing_a_role_records_the_floors_its_value_would_break() {
    let (mut appearance, id) = appearance_with("tilo-dark");
    let kept = theme(&appearance, &id).ink.primary;
    let review =
        |appearance: &mut Replica<AppearancePrefs, AppNamed>, role: &str, value: Option<f64>| {
            appearance
                .apply(
                    AppearancePrefs::REVIEW_THEME.with(super::appearance_prefs::ReviewTheme {
                        theme: "mine".to_owned(),
                        role: role.to_owned(),
                        value,
                    }),
                )
                .unwrap();
        };
    review(&mut appearance, "", None);
    assert!(
        theme(&appearance, &id).findings.is_empty(),
        "a shipped palette keeps every floor"
    );
    // A value the theme admits keeps every floor, so there is nothing to list.
    review(&mut appearance, "ink.primary", Some(0.9));
    assert!(theme(&appearance, &id).findings.is_empty());
    // A value its own edit refuses: the record keeps every pair the tried value would put
    // under its floor, with the ratio it reaches, for the editor to list.
    review(&mut appearance, "ink.primary", Some(0.31));
    let findings = theme(&appearance, &id).findings;
    assert!(!findings.is_empty(), "the pair misses its floor");
    assert!(
        findings.iter().any(|finding| finding.contains(":1")),
        "each finding carries the ratio it reaches: {findings:?}"
    );
    assert!(
        (theme(&appearance, &id).ink.primary - kept).abs() < 1e-6,
        "the theme itself keeps the value it had: the review is of what was tried"
    );
    // Opening the editor without a role lists what the theme misses as it stands.
    review(&mut appearance, "", None);
    assert!(theme(&appearance, &id).findings.is_empty());
}

#[test]
fn a_theme_is_shown_switched_and_deleted_through_its_actions() {
    let (mut appearance, id) = appearance_with("dark");
    appearance
        .apply(
            AppearancePrefs::USE_THEME.with(super::appearance_prefs::UseTheme {
                theme: "mine".to_owned(),
            }),
        )
        .unwrap();
    assert_eq!(appearance.state().unwrap().dark_theme, "custom:mine");
    let settings = appearance.state().unwrap().settings_value();
    let shown = ui::theme::user_document(&settings, ui::theme::local_moment()).expect("a theme");
    assert_eq!(shown.id, "mine");
    assert_eq!(shown.name, "Tilo dark copy");

    appearance
        .apply(AppearancePrefs::SWITCH_POLARITY.with(("mine".to_owned(), false)))
        .unwrap();
    let light = theme(&appearance, &id);
    assert!(!light.dark);
    assert_eq!(light.name, "Tilo dark copy", "switching keeps the name");
    assert_eq!(light.findings("mine"), Vec::<String>::new());
    assert_ne!(
        appearance.state().unwrap().dark_theme,
        "custom:mine",
        "dark mode no longer shows a light theme"
    );

    appearance
        .apply_confirmed(
            AppearancePrefs::DELETE_THEME.with(super::appearance_prefs::DeleteTheme {
                theme: "mine".to_owned(),
            }),
        )
        .unwrap();
    assert!(appearance.state().unwrap().themes.is_empty());
}

#[test]
fn two_people_tuning_different_roles_merge() {
    let (mut ours, id) = appearance_with("dark");
    let mut theirs = ours.fork();
    let paths = AppearancePrefs::paths().themes.item(&id);
    edit(&mut ours, &paths.density.set(1.2)).unwrap();
    edit(&mut theirs, &paths.surfaces.content.set(0.24)).unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let merged = theme(&ours, &id);
    assert!((merged.density - 1.2).abs() < 1e-6);
    assert!((merged.surfaces.content - 0.24).abs() < 1e-6);
    assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
}

#[test]
fn a_new_theme_starts_from_a_preset_or_a_theme_and_ids_are_unique() {
    let (mut appearance, id) = appearance_with("tilo-light");
    edit(
        &mut appearance,
        &AppearancePrefs::paths().themes.item(&id).density.set(1.2),
    )
    .unwrap();
    appearance
        .apply(AppearancePrefs::NEW_THEME.with(("second".to_owned(), "mine".to_owned())))
        .unwrap();
    let second = theme(&appearance, &Id::from_raw("second"));
    assert!((second.density - 1.2).abs() < 1e-6);
    for (new, base) in [
        ("mine", "tilo-dark"),
        ("tilo-dark", "tilo-dark"),
        ("third", "nord"),
    ] {
        assert!(
            appearance
                .apply(AppearancePrefs::NEW_THEME.with((new.to_owned(), base.to_owned())))
                .is_err(),
            "{new} from {base}"
        );
    }
}

#[test]
fn an_imported_theme_is_written_only_when_it_is_admitted_and_legible() {
    let mut appearance = Replica::<AppearancePrefs>::new().in_app("so.tilo.model", "Model");
    let json = |document: &ThemeDocument| serde_json::to_string(document).unwrap();
    let document = Preset::DuskDark.document();
    appearance
        .apply(AppearancePrefs::IMPORT_THEME.with(("ember".to_owned(), json(&document))))
        .unwrap();
    assert_eq!(
        theme(&appearance, &Id::from_raw("ember")).name,
        document.name
    );
    assert!(
        appearance
            .apply(AppearancePrefs::IMPORT_THEME.with(("ember".to_owned(), json(&document))))
            .is_err(),
        "an imported theme cannot overwrite one of yours"
    );

    let mut murky = Preset::Light.document();
    murky.palette.ink.secondary = 0.85;
    let refused = appearance
        .apply(AppearancePrefs::IMPORT_THEME.with(("murky".to_owned(), json(&murky))))
        .expect_err("an illegible theme is not imported");
    assert!(refused.to_string().contains("Too little contrast"));

    assert!(
        appearance
            .apply(
                AppearancePrefs::IMPORT_THEME.with(("broken".to_owned(), "{not json".to_owned()))
            )
            .is_err()
    );
}

/// The refraction a person sets bends floating chrome by that much, and windows and bars in the
/// proportion the theme had them, for the built-in Liquid set and a theme's own glass stage alike;
/// it is refused outside the band a finish admits.
#[test]
fn refraction_bends_every_glass_role_in_proportion() {
    for (json, painter) in [
        (
            include_str!("../../../ui/ui-core/tests/fixtures/finishes/liquid-glass.json"),
            "liquid",
        ),
        (
            include_str!("../../../ui/themes/liquid-glass/theme.json"),
            "shader",
        ),
    ] {
        let document: ThemeDocument = serde_json::from_str(json).unwrap();
        let floating = document.material.floating.param("refraction").unwrap();
        let window = document.material.window.param("refraction").unwrap();
        let mut theme = ThemeRecord::from_document(&document);
        assert!((theme.refraction - f64::from(floating)).abs() < 1e-6);
        theme.refraction = f64::from(floating) * 2.0;
        let edited = theme.document("mine");
        assert_eq!(edited.material.floating.painter(), painter);
        assert_eq!(
            edited.material.floating.param("refraction"),
            Some(floating * 2.0)
        );
        assert_eq!(
            edited.material.window.param("refraction"),
            Some(window * 2.0),
            "{painter}: the window bends in proportion"
        );
        assert_eq!(
            edited.material.floating.shader(),
            document.material.floating.shader(),
            "the glass keeps its stage"
        );
    }
    let (mut appearance, id) = appearance_with("tilo-dark");
    let paths = AppearancePrefs::paths().themes.item(&id);
    assert!(
        edit(&mut appearance, &paths.refraction.set(f64::NAN)).is_err(),
        "a refraction is a number"
    );
}

/// A theme exported from the Themes page (the record, as JSON) and imported on another
/// workspace comes back role by role, tuned values and the glass's own stage included, under an
/// id made from its name when the importer names none.
#[test]
fn an_exported_theme_imports_back_as_it_was() {
    let (mut ours, id) = appearance_with("tilo-dark");
    let paths = AppearancePrefs::paths().themes.item(&id);
    edit(&mut ours, &paths.name.set("Harbour at Night".to_owned())).unwrap();
    edit(&mut ours, &paths.hue.set(Some(210.0))).unwrap();
    let exported = serde_json::to_string(&theme(&ours, &id)).unwrap();

    let mut theirs = Replica::<AppearancePrefs>::new().in_app("so.tilo.model", "Model");
    theirs
        .apply(AppearancePrefs::IMPORT_THEME.with((String::new(), exported)))
        .unwrap();
    let imported = theme(&theirs, &Id::from_raw("harbour-at-night"));
    assert_eq!(imported.name, "Harbour at Night");
    assert_eq!(imported.hue, Some(210.0));
    assert_eq!(
        imported.document("harbour-at-night"),
        theme(&ours, &id).document("harbour-at-night")
    );

    let glass: ThemeDocument =
        serde_json::from_str(include_str!("../../../ui/themes/liquid-glass/theme.json")).unwrap();
    let exported = serde_json::to_string(&ThemeRecord::from_document(&glass)).unwrap();
    theirs
        .apply(AppearancePrefs::IMPORT_THEME.with(("glass".to_owned(), exported)))
        .unwrap();
    assert_eq!(
        theme(&theirs, &Id::from_raw("glass"))
            .document("x")
            .material
            .floating
            .shader(),
        glass.material.floating.shader()
    );
}
