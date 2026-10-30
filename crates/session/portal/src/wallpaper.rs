//! `org.freedesktop.impl.portal.Wallpaper`: an app offering a picture as
//! the person's wallpaper.
//!
//! The picture is read from the app's document-portal export
//! ([`crate::documents`]), shown to the person in a question of kind
//! [`WALLPAPER_QUESTION`], and once they take it kept as their own picture
//! on this device (`tilo_wallpapers::keep_custom_picture`) and chosen
//! (`appearance.wallpaper` = `custom`). The desktop and the lock screen
//! show the same wallpaper, so where the app asked it to go is shown to the
//! person but sets the one picture.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue};

use crate::reply::{Ended, Results, answer};
use crate::{Portal, documents, option};

/// The kind of the question setting a wallpaper asks.
pub const WALLPAPER_QUESTION: &str = "portal.wallpaper";

/// The largest picture taken.
const MAX_PICTURE_BYTES: u64 = 32 * 1024 * 1024;

/// What the sheet shows for a wallpaper an app offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperQuestion {
    /// The picture, as the app's export.
    pub uri: String,
    /// Where the app asked for it: `background`, `lockscreen` or `both`.
    pub set_on: String,
    /// Whether the app asked for a preview.
    pub preview: bool,
}

/// Where option `set-on` asks for the picture; `None` for anything else.
fn set_on(options: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = option::<String>(options, "set-on").unwrap_or_else(|| "both".into());
    matches!(value.as_str(), "background" | "lockscreen" | "both").then_some(value)
}

/// The `org.freedesktop.impl.portal.Wallpaper` interface.
pub struct Wallpaper {
    portal: Portal,
}

impl Wallpaper {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Wallpaper")]
impl Wallpaper {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn set_wallpaper_uri(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        uri: String,
        options: HashMap<String, OwnedValue>,
    ) -> u32 {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, Some(&parent_window))
                .await?;
            let picture = documents::read_exported(&uri, MAX_PICTURE_BYTES).map_err(|error| {
                tracing::debug!(%error, "wallpaper refused");
                Ended::Failed
            })?;
            let question = WallpaperQuestion {
                uri,
                set_on: set_on(&options).ok_or(Ended::Failed)?,
                preview: option(&options, "show-preview").unwrap_or(true),
            };
            let asked = self
                .portal
                .ask(&auth.caller, path, WALLPAPER_QUESTION, question);
            self.portal
                .requests
                .run(connection, path, auth.owner.as_str(), asked)
                .await?;
            tilo_wallpapers::keep_custom_picture(&picture)
                .and_then(|_| {
                    settings_store::appearance::WALLPAPER
                        .set(Some(tilo_wallpapers::CUSTOM.to_owned()))
                })
                .map_err(|error| {
                    tracing::warn!(%error, "wallpaper not set");
                    Ended::Failed
                })?;
            Ok(Results::new())
        })
        .await
        .0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_on_names_only_the_known_places() {
        let with = |value: &str| {
            HashMap::from([(
                "set-on".to_owned(),
                OwnedValue::from(zbus::zvariant::Str::from(value.to_owned())),
            )])
        };
        assert_eq!(set_on(&HashMap::new()).as_deref(), Some("both"));
        assert_eq!(set_on(&with("lockscreen")).as_deref(), Some("lockscreen"));
        assert_eq!(set_on(&with("ceiling")), None);
    }
}
