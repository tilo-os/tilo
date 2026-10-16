//! The toolbar's view of installed extensions: which action buttons show, in
//! what order, with which icon, badge and popup; and the `DevTools` exchanges
//! that read an extension's live action state from its service worker.
//!
//! Chromium keeps an action's badge and title in the browser process, and the
//! engine embeds pages without Chrome's own toolbar, so Tilo reads them the
//! one way the engine allows: briefly attaching to the extension's service
//! worker and calling `chrome.action`. Nothing here touches the engine; it
//! builds requests and parses replies.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::extensions::ExtensionInfo;

/// Chrome shows at most four characters of a badge.
const MAX_BADGE_CHARS: usize = 4;
const MAX_TITLE_BYTES: usize = 256;
/// The button's icon is drawn at toolbar size; larger sources downscale.
const TARGET_ICON_PX: u32 = 32;

/// What an extension's manifest declares for its toolbar action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionManifest {
    pub title: Option<String>,
    /// Popup page, relative to the extension root.
    pub popup: Option<String>,
    /// Best icon file, relative to the extension root.
    pub icon: Option<String>,
}

/// The `action` (MV3) or `browser_action` (MV2) key, reduced to what the
/// toolbar uses. Paths that could leave the extension are dropped.
pub fn action_manifest(manifest: &Value) -> Option<ActionManifest> {
    let action = manifest
        .get("action")
        .or_else(|| manifest.get("browser_action"))?;
    let relative = |value: &Value| {
        let path = value.as_str()?;
        (!path.is_empty()
            && !path.starts_with('/')
            && !path.contains(['\\', ':', '?', '#'])
            && path.split('/').all(|part| part != ".."))
        .then(|| path.to_owned())
    };
    let icon = action
        .get("default_icon")
        .or_else(|| manifest.get("icons"))
        .and_then(|icons| match icons {
            Value::String(_) => relative(icons),
            Value::Object(sizes) => best_icon(
                sizes
                    .iter()
                    .filter_map(|(size, path)| Some((size.parse::<u32>().ok()?, relative(path)?))),
            ),
            _ => None,
        });
    Some(ActionManifest {
        title: action
            .get("default_title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty() && title.len() <= MAX_TITLE_BYTES)
            .map(str::to_owned),
        popup: action.get("default_popup").and_then(relative),
        icon,
    })
}

/// The smallest icon at least toolbar size, else the largest there is.
fn best_icon(sizes: impl Iterator<Item = (u32, String)>) -> Option<String> {
    let mut sizes: Vec<_> = sizes.collect();
    sizes.sort();
    sizes
        .iter()
        .find(|(size, _)| *size >= TARGET_ICON_PX)
        .or_else(|| sizes.last())
        .map(|(_, path)| path.clone())
}

/// One extension, as the toolbar needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The identifier Chromium shows (`chrome-extension://<id>/`).
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub enabled: bool,
    pub pinned: bool,
    pub action: Option<ActionManifest>,
}

impl Installed {
    /// Read an installed extension's manifest.
    pub fn read(info: &ExtensionInfo, enabled: bool, pinned: bool) -> Option<Self> {
        let id = crate::extensions::chrome_id(info)?;
        let manifest = std::fs::read_to_string(info.path.join("manifest.json")).ok()?;
        let manifest: Value = serde_json::from_str(&manifest).ok()?;
        Some(Self {
            id,
            name: info.name.clone(),
            root: info.path.clone(),
            enabled,
            pinned,
            action: action_manifest(&manifest),
        })
    }

    /// `chrome-extension://<id>/<popup>`, when the action has a popup.
    pub fn popup_url(&self) -> Option<String> {
        let popup = self.action.as_ref()?.popup.as_ref()?;
        Some(format!("chrome-extension://{}/{popup}", self.id))
    }

    /// Absolute path of the action icon, when it is inside the extension.
    pub fn icon_path(&self) -> Option<PathBuf> {
        let icon = self.action.as_ref()?.icon.as_ref()?;
        let root = self.root.canonicalize().ok()?;
        let path = root.join(icon).canonicalize().ok()?;
        path.starts_with(&root).then_some(path)
    }
}

/// Live action state read from the service worker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionState {
    pub badge: String,
    pub title: Option<String>,
}

/// One toolbar button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub id: String,
    pub label: String,
    pub tooltip: String,
    pub badge: String,
    pub icon: Option<PathBuf>,
    pub has_popup: bool,
}

/// The toolbar's content: pinned buttons in the row, and every enabled
/// extension with an action in the Extensions menu.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub pinned: Vec<Button>,
    pub menu: Vec<MenuEntry>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub button: Button,
    pub pinned: bool,
}

/// Build the toolbar. Only enabled extensions that declare an action appear;
/// pinned ones keep the order the list gives them.
pub fn model(installed: &[Installed], state_of: impl Fn(&str) -> Option<ActionState>) -> Model {
    let mut model = Model::default();
    for extension in installed.iter().filter(|e| e.enabled) {
        let Some(action) = &extension.action else {
            continue;
        };
        let state = state_of(&extension.id).unwrap_or_default();
        let title = state
            .title
            .or_else(|| action.title.clone())
            .unwrap_or_else(|| extension.name.clone());
        let button = Button {
            id: extension.id.clone(),
            label: extension.name.clone(),
            tooltip: if title == extension.name {
                title
            } else {
                format!("{} \u{2014} {title}", extension.name)
            },
            badge: state.badge,
            icon: extension.icon_path(),
            has_popup: action.popup.is_some(),
        };
        if extension.pinned {
            model.pinned.push(button.clone());
        }
        model.menu.push(MenuEntry {
            button,
            pinned: extension.pinned,
        });
    }
    model
}

/// Chrome's badge presentation: at most four characters.
pub fn badge_text(text: &str) -> String {
    text.chars().take(MAX_BADGE_CHARS).collect()
}

// ---- Service-worker exchange -------------------------------------------

/// `Target.getTargets`.
pub fn targets_request() -> (&'static str, Value) {
    ("Target.getTargets", json!({}))
}

/// The target identifier of `extension`'s service worker among the targets.
pub fn worker_target(body: &str, extension: &str) -> Option<String> {
    let prefix = format!("chrome-extension://{extension}/");
    let targets: Value = serde_json::from_str(body).ok()?;
    targets
        .get("targetInfos")?
        .as_array()?
        .iter()
        .find(|target| {
            target.get("type").and_then(Value::as_str) == Some("service_worker")
                && target
                    .get("url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.starts_with(&prefix))
        })
        .and_then(|target| target.get("targetId")?.as_str().map(str::to_owned))
}

pub fn attach_request(target: &str) -> (&'static str, Value) {
    (
        "Target.attachToTarget",
        json!({"targetId": target, "flatten": true}),
    )
}

pub fn session_id(body: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(body).ok()?;
    let id = reply.get("sessionId")?.as_str()?;
    (!id.is_empty() && id.len() <= 128).then(|| id.to_owned())
}

pub fn detach_request(session: &str) -> (&'static str, Value) {
    ("Target.detachFromTarget", json!({"sessionId": session}))
}

/// Reads the badge and title from `chrome.action`.
const READ_ACTION: &str = "(async()=>{const a=chrome.action||chrome.browserAction;\
const [text,title]=await Promise.all([a.getBadgeText({}),a.getTitle({})]);\
return JSON.stringify({text,title})})()";

pub fn evaluate_request() -> (&'static str, Value) {
    (
        "Runtime.evaluate",
        json!({"expression": READ_ACTION, "awaitPromise": true, "returnByValue": true}),
    )
}

/// The action state in a `Runtime.evaluate` reply.
pub fn action_state(body: &str) -> Option<ActionState> {
    let reply: Value = serde_json::from_str(body).ok()?;
    if reply.get("exceptionDetails").is_some() {
        return None;
    }
    let value = reply.get("result")?.get("value")?.as_str()?;
    let state: Value = serde_json::from_str(value).ok()?;
    Some(ActionState {
        badge: badge_text(state.get("text")?.as_str()?),
        title: state
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty() && title.len() <= MAX_TITLE_BYTES)
            .map(str::to_owned),
    })
}

/// The extension identifier in a `loadUnpacked` reply.
pub fn loaded_id(body: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(body).ok()?;
    let id = reply.get("id")?.as_str()?;
    crate::crx::is_extension_id(id).then(|| id.to_owned())
}

/// Read a PNG action icon inside `root` (see [`Installed::icon_path`]).
pub fn read_icon(path: &Path) -> Option<vello::peniko::ImageData> {
    let length = std::fs::metadata(path).ok()?.len();
    if length > 512 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let decoded = ui::png::decode_rgba8_within(&bytes, 256 * 256).ok()?;
    Some(vello::peniko::ImageData {
        data: vello::peniko::Blob::new(std::sync::Arc::new(decoded.rgba)),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::Alpha,
        width: decoded.width,
        height: decoded.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(id: &str, name: &str, enabled: bool, pinned: bool, manifest: &str) -> Installed {
        Installed {
            id: id.into(),
            name: name.into(),
            root: PathBuf::from("/nonexistent"),
            enabled,
            pinned,
            action: action_manifest(&serde_json::from_str(manifest).unwrap()),
        }
    }

    #[test]
    fn manifest_action_picks_icon_popup_and_refuses_escaping_paths() {
        let manifest: Value = serde_json::from_str(
            r#"{"action":{"default_title":"Lite","default_popup":"popup/index.html",
                "default_icon":{"16":"i16.png","32":"i32.png","128":"i128.png"}}}"#,
        )
        .unwrap();
        let action = action_manifest(&manifest).unwrap();
        assert_eq!(action.title.as_deref(), Some("Lite"));
        assert_eq!(action.popup.as_deref(), Some("popup/index.html"));
        assert_eq!(action.icon.as_deref(), Some("i32.png"));

        // Largest when nothing reaches toolbar size; MV2 and top-level icons.
        let small: Value = serde_json::from_str(
            r#"{"browser_action":{"default_icon":{"16":"a.png","19":"b.png"}}}"#,
        )
        .unwrap();
        assert_eq!(
            action_manifest(&small).unwrap().icon.as_deref(),
            Some("b.png")
        );
        let fallback: Value =
            serde_json::from_str(r#"{"action":{},"icons":{"48":"icon48.png","16":"icon16.png"}}"#)
                .unwrap();
        assert_eq!(
            action_manifest(&fallback).unwrap().icon.as_deref(),
            Some("icon48.png")
        );

        for bad in [
            "../x.png",
            "/abs.png",
            "a/../../b.png",
            "c:\\x.png",
            "x.png?y",
        ] {
            let manifest = json!({"action": {"default_popup": bad, "default_icon": bad}});
            let action = action_manifest(&manifest).unwrap();
            assert_eq!((action.popup, action.icon), (None, None), "{bad}");
        }
        assert!(action_manifest(&json!({"name": "no action"})).is_none());
    }

    #[test]
    fn toolbar_pins_enabled_actions_and_lists_the_rest_in_the_menu() {
        let with_popup = r#"{"action":{"default_popup":"p.html","default_title":"Open"}}"#;
        let list = [
            installed(&"a".repeat(32), "Vault", true, true, with_popup),
            installed(&"b".repeat(32), "Blocker", true, false, r#"{"action":{}}"#),
            installed(&"c".repeat(32), "Disabled", false, true, with_popup),
            installed(&"d".repeat(32), "Headless", true, true, r#"{"name":"x"}"#),
        ];
        let model = model(&list, |id| {
            (id == "b".repeat(32)).then(|| ActionState {
                badge: "12".into(),
                title: Some("12 blocked".into()),
            })
        });
        assert_eq!(model.pinned.len(), 1);
        assert_eq!(model.pinned[0].label, "Vault");
        assert_eq!(model.pinned[0].tooltip, "Vault \u{2014} Open");
        assert!(model.pinned[0].has_popup);
        assert_eq!(model.menu.len(), 2, "disabled and action-less stay out");
        assert!(model.menu[0].pinned && !model.menu[1].pinned);
        assert_eq!(model.menu[1].button.badge, "12");
        assert_eq!(model.menu[1].button.tooltip, "Blocker \u{2014} 12 blocked");
        assert!(!model.menu[1].button.has_popup);
        assert_eq!(
            list[0].popup_url().unwrap(),
            format!("chrome-extension://{}/p.html", "a".repeat(32))
        );
    }

    #[test]
    fn service_worker_replies_parse_and_stay_bounded() {
        let id = "a".repeat(32);
        let targets = json!({"targetInfos":[
            {"targetId":"PAGE","type":"page","url":format!("chrome-extension://{id}/p.html")},
            {"targetId":"OTHER","type":"service_worker","url":format!("chrome-extension://{}/sw.js", "b".repeat(32))},
            {"targetId":"WORKER","type":"service_worker","url":format!("chrome-extension://{id}/sw.js")}
        ]})
        .to_string();
        assert_eq!(worker_target(&targets, &id).as_deref(), Some("WORKER"));
        assert_eq!(worker_target(&targets, &"c".repeat(32)), None);
        assert_eq!(worker_target("not json", &id), None);

        assert_eq!(session_id(r#"{"sessionId":"S-1"}"#).as_deref(), Some("S-1"));
        assert_eq!(
            session_id(&json!({"sessionId": "x".repeat(129)}).to_string()),
            None
        );

        let evaluated = json!({"result":{"type":"string","value":
            json!({"text":"123456","title":"6 blocked"}).to_string()}})
        .to_string();
        assert_eq!(
            action_state(&evaluated),
            Some(ActionState {
                badge: "1234".into(),
                title: Some("6 blocked".into())
            })
        );
        assert_eq!(action_state(r#"{"exceptionDetails":{}}"#), None);
        assert_eq!(action_state(r#"{"result":{"value":"{"}}"#), None);

        assert_eq!(loaded_id(&json!({"id": id}).to_string()), Some(id));
        assert_eq!(loaded_id(r#"{"id":"../x"}"#), None);
        assert_eq!(evaluate_request().0, "Runtime.evaluate");
        assert_eq!(
            attach_request("T").1,
            json!({"targetId":"T","flatten":true})
        );
    }
}
