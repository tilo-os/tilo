//! The studio's platform effects, shared by controls and the resource door.
use crate::Studio;
use ui::framework::{Effected, ViewCx};
use ui::prelude::*;
use ui::theme::{Preset, ThemeDocument};

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "themes.effect")]
pub enum ThemesEffect {
    /// Apply as system theme.
    #[effect(job, cancellable, device_only)]
    ApplyTheme {
        #[param("Id of the preset or written theme")]
        theme: String,
    },
    /// Export theme JSON.
    #[effect(returns, kind = "read")]
    Export {
        #[param("Id of the preset or written theme")]
        theme: String,
    },
}

fn document(studio: &Studio, id: &str) -> Result<ThemeDocument, String> {
    Preset::find(id)
        .map(Preset::document)
        .or_else(|| {
            studio
                .themes
                .get(&Id::from_raw(id))
                .map(|theme| theme.document(id))
        })
        .ok_or_else(|| format!("There is no theme called {id}."))
}

pub(crate) fn install(cx: &ViewCx<Studio>, notice: &LocalState<String>) {
    let notice = notice.clone();
    cx.effects::<ThemesEffect>(move |effect, cx| {
        let id = match &effect {
            ThemesEffect::ApplyTheme { theme } | ThemesEffect::Export { theme } => theme,
        };
        let document = cx.live().with(|studio| document(studio, id))?;
        crate::apply::ensure_legible(&document)?;
        match effect {
            ThemesEffect::ApplyTheme { .. } => {
                let notice = notice.clone();
                let name = document.name.clone();
                Ok(cx.thread(
                    move |_| crate::apply::apply(&document),
                    move |(), result, _| match result {
                        Ok(()) => {
                            notice.set(format!("{name} is now your theme."));
                            Ok(Effected::done())
                        }
                        Err(reason) => {
                            notice.set(reason.clone());
                            Err(reason)
                        }
                    },
                ))
            }
            ThemesEffect::Export { .. } => {
                let json = crate::apply::export(&document)?;
                ui::text_selection::clipboard_copy(&json);
                notice.set("The theme is on the clipboard.".to_owned());
                Ok(Effected::value(json))
            }
        }
    });
}
