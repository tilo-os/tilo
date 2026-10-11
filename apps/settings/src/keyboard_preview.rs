//! Native XKB legends over explicitly selected Open Group PC geometry.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;
use ui::{KeyboardMap, KeyboardMapKey};
use vello::kurbo::Point;

#[derive(Deserialize)]
struct Geometry {
    width: f64,
    height: f64,
    keys: Vec<GeometryKey>,
}
#[derive(Deserialize)]
struct GeometryKey {
    name: String,
    outline: Vec<[f64; 2]>,
}

pub(crate) fn valid_model(model: &str) -> bool {
    matches!(model, "pc104" | "pc105" | "jp106" | "kr106")
}

/// Build a visual map from installed native XKB data and the same bounded
/// keyboard options the compositor reads. Does not apply settings or capture input.
///
/// # Errors
/// Rejects invalid sources or models and unavailable or invalid installed metadata.
pub fn preview_layout(
    source: &str,
    model: &str,
    policy: &settings_store::hardware::RichInputConfig,
    label: String,
) -> Result<KeyboardMap, String> {
    map_with_options(source, model, label, Some(&policy.xkb_options().join(",")))
}

pub(crate) fn map_with_options(
    source: &str,
    model: &str,
    label: String,
    options: Option<&str>,
) -> Result<KeyboardMap, String> {
    static GEOMETRIES: LazyLock<Result<BTreeMap<String, Geometry>, serde_json::Error>> =
        LazyLock::new(|| serde_json::from_str(include_str!("../data/pc-keyboard-geometry.json")));
    let source_id = source.to_owned();
    let source = settings_store::hardware::KeyboardInputSource::parse(source)
        .ok_or_else(|| "the source is not an XKB layout".to_owned())?;
    let geometries = GEOMETRIES
        .as_ref()
        .map_err(|_| "keyboard geometry is invalid")?;
    let geometry = geometries
        .get(model)
        .ok_or("unknown physical keyboard model")?;
    let keys = compile(&source, model, &geometry.keys, options)?;
    Ok(KeyboardMap::new(
        keys,
        taffy::Size {
            width: geometry.width,
            height: geometry.height,
        },
        label,
    )
    .with_configuration(source_id, model.to_owned()))
}

#[cfg(test)]
thread_local! { static COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
pub(crate) fn compilation_count() -> usize {
    COMPILES.with(std::cell::Cell::get)
}

fn compile(
    source: &settings_store::hardware::KeyboardInputSource,
    model: &str,
    geometry: &[GeometryKey],
    options: Option<&str>,
) -> Result<Vec<KeyboardMapKey>, String> {
    #[cfg(test)]
    COMPILES.with(|count| count.set(count.get() + 1));
    let native = tilo_keyboard_map::compile(
        &source.layout,
        source.variant.as_deref().unwrap_or(""),
        model,
        options.unwrap_or(""),
        &geometry
            .iter()
            .map(|key| key.name.clone())
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let caps = ui::Keycaps::from_xkb(
        source.layout.as_str(),
        source.variant.as_deref().unwrap_or(""),
    );
    native
        .into_iter()
        .zip(geometry)
        .map(|(key, outline)| {
            let levels = key
                .levels
                .into_iter()
                .map(|symbols| {
                    symbols
                        .into_iter()
                        .map(|symbol| caps.native_symbol(&symbol.name, &symbol.text))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            Ok(KeyboardMapKey {
                name: key.name,
                physical: key.physical,
                outline: outline
                    .outline
                    .iter()
                    .map(|&[x, y]| Point::new(x, y))
                    .collect(),
                levels,
            })
        })
        .collect()
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, PhysicalKey};
    #[test]
    fn jis_and_korean_models_preserve_the_native_language_key_positions() {
        let geometry: BTreeMap<String, Geometry> =
            serde_json::from_str(include_str!("../data/pc-keyboard-geometry.json")).unwrap();
        for (source, model, positions) in [
            (
                "jp",
                "jp106",
                vec![
                    ("AE13", KeyCode::IntlYen),
                    ("AB11", KeyCode::IntlRo),
                    ("MUHE", KeyCode::NonConvert),
                    ("HENK", KeyCode::Convert),
                    ("HKTG", KeyCode::KanaMode),
                ],
            ),
            (
                "kr",
                "kr106",
                vec![("HNGL", KeyCode::Lang1), ("HJCV", KeyCode::Lang2)],
            ),
        ] {
            assert_eq!(geometry[model].keys.len(), 106);
            let source = settings_store::hardware::KeyboardInputSource::parse(source).unwrap();
            let keys = compile(&source, model, &geometry[model].keys, Some("")).unwrap();
            for (name, physical) in positions {
                let key = keys.iter().find(|key| key.name == name).unwrap();
                assert_eq!(key.physical, PhysicalKey::Code(physical));
                assert!(
                    !key.levels.is_empty(),
                    "{model}/{name} must have native symbols"
                );
            }
        }
    }

    #[test]
    fn every_actual_installed_xkb_source_compiles_for_each_offered_physical_model() {
        assert!(
            crate::input_sources::layout_catalog_error().is_none(),
            "missing installed catalogue must not become fallback success"
        );
        let catalog = crate::input_sources::installed_catalog();
        let mut observed = 0;
        for entry in catalog {
            if settings_store::hardware::KeyboardInputSource::parse(&entry.id).is_none() {
                continue;
            }
            for model in ["pc104", "pc105", "jp106", "kr106"] {
                map_with_options(&entry.id, model, String::new(), Some(""))
                    .unwrap_or_else(|error| panic!("{}/{model}: {error}", entry.id));
            }
            observed += 1;
        }
        assert!(
            observed > 100,
            "the proof must observe the installed full catalogue"
        );
    }

    #[test]
    fn installed_xkb_legends_keep_physical_positions_across_layouts_and_models() {
        let geometry: BTreeMap<String, Geometry> =
            serde_json::from_str(include_str!("../data/pc-keyboard-geometry.json")).unwrap();
        assert_eq!(geometry["pc104"].keys.len(), 104);
        assert_eq!(geometry["pc105"].keys.len(), 105);
        assert!(!geometry["pc104"].keys.iter().any(|key| key.name == "LSGT"));
        assert_eq!(
            geometry["pc105"]
                .keys
                .iter()
                .find(|key| key.name == "RTRN")
                .unwrap()
                .outline
                .len(),
            6
        );
        let at = |source: &str, name: &str| {
            let source = settings_store::hardware::KeyboardInputSource::parse(source).unwrap();
            compile(&source, "pc105", &geometry["pc105"].keys, Some(""))
                .unwrap()
                .into_iter()
                .find(|key| key.name == name)
                .unwrap()
        };
        let us = at("us", "AD06");
        let de = at("de", "AD06");
        assert_eq!(us.physical, PhysicalKey::Code(KeyCode::KeyY));
        assert_eq!(de.physical, us.physical);
        assert_eq!(us.levels.first().map(String::as_str), Some("y"));
        assert_eq!(de.levels.first().map(String::as_str), Some("z"));
        let dead = at("de", "AE12");
        assert_eq!(dead.levels.first().map(String::as_str), Some("◌́"));
        let greek = at("gr", "AD06");
        assert_eq!(greek.physical, us.physical);
        assert_eq!(greek.levels.first().map(String::as_str), Some("υ"));
        let dvorak = at("us:dvorak", "AD01");
        assert_eq!(dvorak.physical, PhysicalKey::Code(KeyCode::KeyQ));
        assert_eq!(dvorak.levels.first().map(String::as_str), Some("'"));
        assert!(map_with_options("ime/rime", "pc105", String::new(), Some("")).is_err());
        assert!(map_with_options("us", "../../attacker", String::new(), Some("")).is_err());
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod sandbox_tests {
    #[test]
    fn native_preview_uses_the_normal_readonly_sandbox_profile() {
        const CHILD: &str = "TILO_KEYBOARD_PREVIEW_PROFILE_CHILD";
        if std::env::var(CHILD).as_deref() == Ok("1") {
            let ro = launcher::profile::landlock_ro_paths(false);
            let plan = launcher::landlock::plan_from_paths(&ro, &[]);
            assert_eq!(
                launcher::landlock::apply_plan_or_log(&plan, true),
                launcher::landlock::LandlockOutcome::FullyEnforced
            );
            assert!(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open("/usr/share/X11/xkb/rules/evdev.xml")
                    .is_err()
            );
            let catalog = crate::input_sources::installed_catalog();
            assert!(catalog.iter().any(|entry| entry.id == "us"));
            assert!(catalog.iter().any(|entry| entry.id == "de"));
            super::map_with_options("de", "pc105", String::new(), Some(""))
                .expect("the native compiler works inside the ordinary readonly profile");
            assert!(
                std::fs::read("/run/private-probe").is_err(),
                "Landlock does not grant all /run"
            );
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let private = temporary.path().join("private");
        std::fs::write(&private, b"owned fixture only").unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut args = vec![
            "--unshare-all".into(),
            "--die-with-parent".into(),
            "--new-session".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--tmpfs".into(),
            "/tmp".into(),
        ];
        launcher::sandbox::push_clean_environment(&mut args);
        launcher::profile::push_system_profile(&mut args, false);
        args.extend(["--dir".into(), "/run".into(), "--ro-bind".into(),
            executable.to_str().unwrap().into(), "/run/keyboard-probe".into(),
            "--ro-bind".into(), private.to_str().unwrap().into(), "/run/private-probe".into(),
            "--setenv".into(), CHILD.into(), "1".into(), "--".into(),
            "/run/keyboard-probe".into(), "--exact".into(),
            "keyboard_preview::sandbox_tests::native_preview_uses_the_normal_readonly_sandbox_profile".into(),
            "--nocapture".into()]);
        let output = std::process::Command::new("bwrap")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "actual readonly-profile child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
