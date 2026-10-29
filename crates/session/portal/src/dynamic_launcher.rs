//! `org.freedesktop.impl.portal.DynamicLauncher`: an app adding a launcher
//! of its own (a web app, a shortcut into it) to the person's apps.
//!
//! The backend is the confirmation: `PrepareInstall` asks a question of
//! kind [`LAUNCHER_QUESTION`] showing the name, the icon and, for a web
//! app, the site; the person may rename it (the answer's first typed value)
//! and pick a plain icon instead of the app's (the answer's choice). The
//! frontend then writes the entry under
//! `$XDG_DATA_HOME/xdg-desktop-portal/applications`, which the launcher and
//! search read like any installed app. An app is never handed a token to
//! install without asking.
//!
//! Only icons as bytes are taken (`('bytes', <ay>)`, as `GLib` serialises
//! one), bounded PNG/JPEG or a script-free SVG: never a path or a file
//! descriptor the session would read on the app's behalf.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::interface;
use zbus::zvariant::{ObjectPath, OwnedValue, Str, Structure, Value};

use crate::reply::{Ended, Reply, Results, answer};
use crate::{Portal, option, text_option};

/// The kind of the question adding a launcher asks.
pub const LAUNCHER_QUESTION: &str = "portal.launcher";

/// The launcher kinds: an app's own launcher, a web app.
const APPLICATION: u32 = 1;
const WEBAPP: u32 = 2;

/// What the sheet shows for adding a launcher.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherQuestion {
    /// The name the app proposes.
    pub name: String,
    /// Whether the person may rename it.
    pub editable_name: bool,
    /// Whether the person may pick one of the plain icons instead
    /// (choices 1 and 2; 0 keeps the app's).
    pub editable_icon: bool,
    /// The app's icon, as a `data:` URI.
    pub icon: String,
    /// The site a web app opens.
    pub target: Option<String>,
}

const MONOGRAM_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><rect width="128" height="128" rx="28" fill="#2d6cdf"/><path fill="white" d="M27 38h74v16H43v20h45v16H43v16h58v16H27z"/></svg>"##;
const WEB_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><rect width="128" height="128" rx="28" fill="#087f5b"/><circle cx="64" cy="64" r="36" fill="none" stroke="white" stroke-width="8"/><path fill="none" stroke="white" stroke-width="8" d="M28 64h72M64 28c14 16 14 56 0 72M64 28c-14 16-14 56 0 72"/></svg>"##;

/// An icon as `GLib` serialises one held in bytes.
fn bytes_icon(bytes: Vec<u8>) -> Option<OwnedValue> {
    OwnedValue::try_from(Value::new(("bytes", Value::new(bytes)))).ok()
}

/// The bytes of a `('bytes', <ay>)` icon; `None` for any other form.
fn icon_bytes(icon: &OwnedValue) -> Option<Vec<u8>> {
    if icon.value_signature().to_string().contains('h') {
        return None;
    }
    let structure = Structure::try_from(icon.try_clone().ok()?).ok()?;
    let [kind, bytes]: [Value<'_>; 2] = structure.into_fields().try_into().ok()?;
    (String::try_from(kind).ok()? == "bytes").then_some(())?;
    bytes.downcast::<Vec<u8>>().ok()
}

/// Whether `bytes` is an icon the session will show: a PNG or JPEG of at
/// most 512 pixels a side, or an SVG that runs nothing and loads nothing.
pub fn valid_icon_payload(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
        return false;
    }
    if let Some((format, width, height)) = tilo_content_types::image_size(bytes) {
        return matches!(format.extension, "png" | "jpg") && width <= 512 && height <= 512;
    }
    if bytes.len() > 256 * 1024 {
        return false;
    }
    let Ok(svg) = std::str::from_utf8(bytes) else {
        return false;
    };
    let lower = svg.to_ascii_lowercase();
    lower.trim_start().starts_with("<svg")
        && ![
            "<script",
            "<foreignobject",
            "href=\"http:",
            "href='http:",
            "href=\"https:",
            "href='https:",
            "href=\"file:",
            "href='file:",
            "href=\"data:",
            "href='data:",
            "url(http",
            "url(file",
            "url(data",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// A web app's site: `https://` and a host, plain text.
fn safe_web_target(target: &str) -> bool {
    target
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && target.trim() == target
        && crate::plain_text(target, 4096)
}

/// A launcher name: not blank, no padding, bounded plain text.
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.trim() == name && crate::plain_text(name, 255)
}

/// The icon as a `data:` URI the sheet can show.
fn data_uri(bytes: &[u8]) -> String {
    let mime =
        tilo_content_types::image_size(bytes).map_or("image/svg+xml", |(format, _, _)| format.mime);
    format!(
        "data:{mime};base64,{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
    )
}

/// The `org.freedesktop.impl.portal.DynamicLauncher` interface.
pub struct DynamicLauncher {
    portal: Portal,
    /// The connection it is served on, where each request's object lives.
    connection: zbus::Connection,
}

impl DynamicLauncher {
    #[must_use]
    pub const fn new(portal: Portal, connection: zbus::Connection) -> Self {
        Self { portal, connection }
    }
}

#[interface(name = "org.freedesktop.impl.portal.DynamicLauncher")]
impl DynamicLauncher {
    #[zbus(property, name = "SupportedLauncherTypes")]
    const fn supported_launcher_types(&self) -> u32 {
        APPLICATION | WEBAPP
    }

    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn prepare_install(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        name: String,
        icon: OwnedValue,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async {
            let path = handle.as_str();
            let auth = self
                .portal
                .authenticate(&header, path, &app_id, Some(&parent_window))
                .await?;
            let supplied = icon_bytes(&icon)
                .filter(|bytes| valid_icon_payload(bytes))
                .ok_or(Ended::Failed)?;
            let kind = option::<u32>(&options, "launcher_type").unwrap_or(APPLICATION);
            let target = text_option(&options, "target", 4096);
            let valid = valid_name(&name)
                && match kind {
                    APPLICATION => target.is_none(),
                    WEBAPP => target.as_deref().is_some_and(safe_web_target),
                    _ => false,
                };
            if !valid {
                return Err(Ended::Failed);
            }
            let question = LauncherQuestion {
                name: name.clone(),
                editable_name: option(&options, "editable_name").unwrap_or(true),
                editable_icon: option(&options, "editable_icon").unwrap_or(false),
                icon: data_uri(&supplied),
                target,
            };
            let (editable_name, editable_icon) = (question.editable_name, question.editable_icon);
            let asked = self
                .portal
                .ask(&auth.caller, path, LAUNCHER_QUESTION, question);
            let answered = self
                .portal
                .requests
                .run(&self.connection, path, auth.owner.as_str(), asked)
                .await?;
            let name = match answered.values.first() {
                Some(typed) if editable_name => valid_name(typed)
                    .then(|| typed.to_string())
                    .ok_or(Ended::Cancelled)?,
                _ => name,
            };
            let icon = match answered.choice {
                Some(1) if editable_icon => bytes_icon(MONOGRAM_ICON.as_bytes().to_vec()),
                Some(2) if editable_icon => bytes_icon(WEB_ICON.as_bytes().to_vec()),
                _ => Some(icon),
            }
            .ok_or(Ended::Failed)?;
            let mut results = Results::new();
            results.insert("name".into(), OwnedValue::from(Str::from(name)));
            results.insert("icon".into(), icon);
            Ok(results)
        })
        .await
    }

    /// No app installs a launcher without the person seeing it.
    fn request_install_token(&self, app_id: String, options: HashMap<String, OwnedValue>) -> u32 {
        drop((app_id, options));
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_sites_and_byte_icons_are_taken() {
        assert!(safe_web_target("https://example.test"));
        assert!(!safe_web_target("https:///path"));
        assert!(!safe_web_target("http://example.test"));
        assert!(!safe_web_target("file:///tmp/a"));
        let icon = bytes_icon(MONOGRAM_ICON.as_bytes().to_vec()).unwrap();
        assert!(icon_bytes(&icon).is_some_and(|bytes| valid_icon_payload(&bytes)));
        let path_icon = OwnedValue::try_from(Value::new((
            "file",
            Value::new("/home/person/.ssh/id_ed25519"),
        )))
        .unwrap();
        assert_eq!(icon_bytes(&path_icon), None);
    }

    #[test]
    fn scripted_or_remote_svgs_are_refused() {
        assert!(!valid_icon_payload(b"<svg><script>alert(1)</script></svg>"));
        assert!(!valid_icon_payload(
            b"<svg><image href=\"https://x/y.png\"/></svg>"
        ));
        assert!(valid_icon_payload(WEB_ICON.as_bytes()));
        assert!(data_uri(WEB_ICON.as_bytes()).starts_with("data:image/svg+xml;base64,"));
    }

    #[test]
    fn names_are_plain_and_not_blank() {
        assert!(valid_name("Notes"));
        assert!(!valid_name(" Notes"));
        assert!(!valid_name("   "));
        assert!(!valid_name("a\nb"));
    }
}
