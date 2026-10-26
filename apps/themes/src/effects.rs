//! The studio's platform effects, shared by controls and the resource door.
use crate::Studio;
use ui::framework::{Effected, ViewCx};
use ui::prelude::*;
use ui::theme::{Preset, ThemeDocument};

/// The largest theme file the importer reads.
const MAX_THEME_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "themes.effect")]
pub enum ThemesEffect {
    /// Apply as system theme.
    ///
    /// The person's own appearance on their own device: nothing shared, so
    /// a guest applies one as the room's owner does.
    #[effect(job, cancellable, own_device)]
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
    /// Import a theme file through the host's consented file chooser.
    #[effect(attended, job, cancellable)]
    Import,
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

fn imported_notice(name: &str) -> String {
    crate::locales::CATALOG.t_args(
        "themes-imported",
        &[("name", ui::ArgValue::Text(name.to_owned()))],
    )
}

fn refused_notice(name: &str, reason: &str) -> String {
    crate::locales::CATALOG.t_args(
        "themes-import-failed",
        &[
            ("name", ui::ArgValue::Text(name.to_owned())),
            ("reason", ui::ArgValue::Text(reason.to_owned())),
        ],
    )
}

pub(crate) fn install(
    cx: &ViewCx<Studio>,
    notice: &LocalState<String>,
    page: &LocalState<crate::view::Page>,
    pane: &LocalState<NavPane>,
) {
    let notice = notice.clone();
    let page = page.clone();
    let pane = pane.clone();
    cx.effects::<ThemesEffect>(move |effect, cx| {
        if effect == ThemesEffect::Import {
            let notice = notice.clone();
            let page = page.clone();
            let pane = pane.clone();
            return Ok(cx.choose_file(
                ui::portal::PickRequest::open(
                    &crate::locales::CATALOG.t("themes-import-title"),
                    &crate::locales::CATALOG.t("themes-import"),
                )
                .filter(
                    ui::portal::FileFilter::new(&crate::locales::CATALOG.t("themes-import-files"))
                        .mimetype("application/json")
                        .glob("*.json"),
                )
                .max_contents_bytes(MAX_THEME_BYTES),
                move |_, chosen, cx| {
                    let Some(file) = chosen else {
                        return Ok(Effected::done());
                    };
                    let text = String::from_utf8(file.bytes).map_err(|_| {
                        let reason = format!("{} is not a theme document", file.name);
                        notice.set(refused_notice(&file.name, &reason));
                        reason
                    })?;
                    let document: ThemeDocument = serde_json::from_str(&text).map_err(|error| {
                        let reason = format!("{} is not a theme document: {error}", file.name);
                        notice.set(refused_notice(&file.name, &reason));
                        reason
                    })?;
                    if let Err(reason) = crate::apply::ensure_legible(&document) {
                        notice.set(refused_notice(&file.name, &reason));
                        return Ok(Effected::done());
                    }
                    let base = document.id.trim();
                    let base = if base.is_empty() { "imported" } else { base };
                    let mut id = base.to_owned();
                    let mut number = 2;
                    while cx.live().with(|studio| {
                        studio.themes.contains(&Id::from_raw(id.clone()))
                            || Preset::find(&id).is_some()
                    }) {
                        id = format!("{base}-{number}");
                        number += 1;
                    }
                    let json = serde_json::to_string(&document).unwrap_or_default();
                    cx.submit(Studio::IMPORT_THEME.with((id.clone(), json)));
                    notice.set(imported_notice(&document.name));
                    page.set(crate::view::Page::Theme(id));
                    pane.set(NavPane::Content);
                    Ok(Effected::done())
                },
            ));
        }
        let id = match &effect {
            ThemesEffect::ApplyTheme { theme } | ThemesEffect::Export { theme } => theme,
            ThemesEffect::Import => return Ok(Effected::done()),
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
            ThemesEffect::Import => Ok(Effected::done()),
        }
    });
}
