//! A capture's settings: what its private profile stores before the app
//! reads it.

use serde_json::{Map, Value};

use crate::theme::Preset;
use crate::theme_layers::{DARK_THEME_KEY, LIGHT_THEME_KEY, MODE_KEY};

/// The `appearance` section a capture's profile stores: the theme the
/// capture draws in (`--theme`, else `TILO_SHOTS_THEME`, else dark) as the
/// person's choice, so a control showing the stored theme shows what is on
/// screen.
#[must_use]
pub fn capture_appearance(args: &super::LaunchArgs) -> Map<String, Value> {
    appearance(
        super::shot_theme_name(args)
            .as_deref()
            .and_then(Preset::find)
            .unwrap_or(Preset::Dark),
    )
}

/// The `appearance` section that stores `preset` as the person's theme.
fn appearance(preset: Preset) -> Map<String, Value> {
    let dark = preset.document().is_dark();
    let mut appearance = Map::new();
    appearance.insert(
        MODE_KEY.into(),
        Value::from(if dark { "dark" } else { "light" }),
    );
    appearance.insert(
        if dark {
            DARK_THEME_KEY
        } else {
            LIGHT_THEME_KEY
        }
        .into(),
        Value::from(preset.id()),
    );
    appearance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_stores_the_theme_it_draws_in() {
        for (name, preset) in [
            ("light", Preset::Light),
            ("dark", Preset::Dark),
            ("dark-graphite", Preset::DarkGraphite),
        ] {
            let args = super::super::LaunchArgs {
                theme: Some(name.to_owned()),
                ..super::super::LaunchArgs::default()
            };
            let mut settings = Map::new();
            settings.insert(
                "appearance".into(),
                Value::Object(capture_appearance(&args)),
            );
            let shown = crate::theme_layers::user_document(
                &Value::Object(settings),
                settings_store::day_schedule::LocalMoment {
                    day_of_year: 1,
                    minute_of_day: 720,
                    utc_offset_minutes: 0,
                },
            )
            .expect("the stored appearance names a theme");
            assert_eq!(
                shown.id,
                preset.id(),
                "{name}: the stored theme is the one drawn"
            );
        }
    }
}
