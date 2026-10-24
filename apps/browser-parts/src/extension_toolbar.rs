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
use ui::webview::ExtensionPermissionSet;

use crate::extensions::ExtensionInfo;

/// Chrome shows at most four characters of a badge.
const MAX_BADGE_CHARS: usize = 4;
/// What Chromium answers for the badge of every tab while an extension's
/// button shows each page's count of what its rules did there
/// (`declarativeNetRequest`): no text of its own. The engine tells each
/// page's count for that page's tab (`extension_action`).
const RULE_COUNT_PLACEHOLDER: &str = "<<declarativeNetRequestActionCount>>";
const MAX_TITLE_BYTES: usize = 256;
/// The button's icon is drawn at toolbar size; larger sources downscale.
const TARGET_ICON_PX: u32 = 32;

/// What an extension's manifest declares for its toolbar action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionManifest {
    pub title: Option<String>,
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

/// What an extension may do with the Browser's tabs, as its manifest asks:
/// its API permissions (`tabs`, `activeTab`, `scripting`), the sites it may
/// read and script, and apart from those the sites its content scripts run
/// on. As in Chrome, a content script's sites give no access to the tab
/// APIs: an extension whose content scripts match every page sees no tab's
/// address, scripts no page and captures none without asking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub permissions: Vec<String>,
    /// Its host permissions (MV2: the sites among its permissions).
    pub hosts: Vec<String>,
    /// Where its declared content scripts run.
    pub scriptable: Vec<String>,
    /// The token its service worker shows the engine
    /// ([`crate::extensions::prepare_tabs`]).
    pub token: Option<String>,
    /// Its keyboard shortcuts: the manifest's `commands`, whose suggested
    /// keys the engine gives and runs as Chrome does.
    pub commands: Option<Value>,
    /// What it may ask for later (`optional_permissions`,
    /// `optional_host_permissions`), and what of that the person granted
    /// ([`crate::extensions::granted`]).
    pub optional: ExtensionPermissionSet,
    pub granted: ExtensionPermissionSet,
}

pub fn access(manifest: &Value) -> Access {
    let strings = |value: Option<&Value>| -> Vec<String> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let site = |entry: &String| entry == "<all_urls>" || entry.contains("://");
    // Manifest V2 lists optional sites among its optional permissions too.
    let mut optional = ExtensionPermissionSet {
        permissions: Vec::new(),
        origins: strings(manifest.get("optional_host_permissions")),
    };
    for entry in strings(manifest.get("optional_permissions")) {
        if site(&entry) {
            optional.origins.push(entry);
        } else {
            optional.permissions.push(entry);
        }
    }
    let mut permissions = Vec::new();
    let mut hosts = strings(manifest.get("host_permissions"));
    let mut scriptable = Vec::new();
    // Manifest V2 lists sites among its permissions.
    for entry in strings(manifest.get("permissions")) {
        if site(&entry) {
            hosts.push(entry);
        } else {
            permissions.push(entry);
        }
    }
    for script in manifest
        .get("content_scripts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        scriptable.extend(strings(script.get("matches")));
    }
    for sites in [&mut hosts, &mut scriptable] {
        sites.sort();
        sites.dedup();
    }
    Access {
        permissions,
        hosts,
        scriptable,
        token: None,
        commands: manifest
            .get("commands")
            .filter(|commands| commands.is_object())
            .cloned(),
        optional,
        granted: ExtensionPermissionSet::default(),
    }
}

/// An extension's settings page (`options_ui.page`, or MV2's
/// `options_page`), relative to its root; one that could leave it is none.
pub fn options_page(manifest: &Value) -> Option<String> {
    manifest
        .get("options_ui")
        .and_then(|options| options.get("page"))
        .or_else(|| manifest.get("options_page"))
        .and_then(Value::as_str)
        .filter(|page| {
            !page.is_empty()
                && !page.starts_with('/')
                && !page.contains(['\\', ':', '?', '#'])
                && page.split('/').all(|part| part != "..")
        })
        .map(str::to_owned)
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
    /// Its settings page, relative to its root (`options_ui.page` or MV2's
    /// `options_page`): where an ad blocker keeps its lists and allowed sites.
    pub options: Option<String>,
    pub access: Access,
    /// Its keyword in the address (`omnibox.keyword`): typed with a space,
    /// what follows is the extension's ([`crate::extension_omnibox`]).
    pub keyword: Option<String>,
}

impl Installed {
    pub fn read(info: &ExtensionInfo, enabled: bool, pinned: bool) -> Option<Self> {
        let id = crate::extensions::chrome_id(info)?;
        let manifest = std::fs::read_to_string(info.path.join("manifest.json")).ok()?;
        let manifest: Value = serde_json::from_str(&manifest).ok()?;
        let mut action = action_manifest(&manifest);
        // A title is named as the extension is: in its default locale.
        if let Some(title) = action.as_mut().and_then(|action| action.title.as_mut())
            && title.contains("__MSG_")
        {
            let messages = crate::extensions::messages_file(&manifest)
                .and_then(|file| crate::extensions::read_bounded(&info.path.join(file)))
                .map(|text| crate::extensions::messages(&text))
                .unwrap_or_default();
            *title = crate::extensions::localize(title, &messages);
        }
        let options = options_page(&manifest);
        let access = Access {
            token: crate::extensions::tabs_token_of(&info.path),
            granted: crate::extensions::granted(info),
            ..access(&manifest)
        };
        Some(Self {
            id,
            name: info.name.clone(),
            root: info.path.clone(),
            enabled,
            pinned,
            action,
            options,
            access,
            keyword: crate::extension_omnibox::manifest_keyword(&manifest),
        })
    }

    /// `chrome-extension://<id>/<popup>`, when the action has a popup.
    pub fn popup_url(&self) -> Option<String> {
        let popup = self.action.as_ref()?.popup.as_ref()?;
        Some(format!("chrome-extension://{}/{popup}", self.id))
    }

    /// `chrome-extension://<id>/<settings page>`, when it has one.
    pub fn options_url(&self) -> Option<String> {
        Some(format!(
            "chrome-extension://{}/{}",
            self.id,
            self.options.as_ref()?
        ))
    }

    /// Absolute path of the action icon, when it is inside the extension.
    pub fn icon_path(&self) -> Option<PathBuf> {
        let icon = self.action.as_ref()?.icon.as_ref()?;
        let root = self.root.canonicalize().ok()?;
        let path = root.join(icon).canonicalize().ok()?;
        path.starts_with(&root).then_some(path)
    }
}

/// An action's state as Chromium holds it for every tab: read from the
/// service worker, or told by the extension's script as it changed it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionState {
    pub badge: String,
    pub title: Option<String>,
    /// The popup's address; empty when a click goes to the extension, `None`
    /// until known, when the manifest's stands.
    pub popup: Option<String>,
    pub disabled: bool,
}

impl ActionState {
    /// Take in what the extension changed for every tab.
    pub fn apply(&mut self, change: &ui::webview::ExtensionActionChange) {
        if let Some(badge) = &change.badge {
            self.badge = badge_text(badge.as_deref().unwrap_or_default());
        }
        if let Some(title) = &change.title {
            self.title = title.as_deref().and_then(bounded_title);
        }
        if let Some(popup) = &change.popup {
            self.popup.clone_from(popup);
        }
        if let Some(enabled) = change.enabled {
            self.disabled = enabled == Some(false);
        }
    }

    /// This state with what the extension set for one page over it.
    #[must_use]
    pub fn under(mut self, page: &PageValues) -> Self {
        if let Some(badge) = &page.badge {
            self.badge.clone_from(badge);
        }
        if let Some(title) = &page.title {
            self.title = Some(title.clone()).filter(|title| !title.is_empty());
        }
        if let Some(popup) = &page.popup {
            self.popup = Some(popup.clone());
        }
        if let Some(enabled) = page.enabled {
            self.disabled = !enabled;
        }
        self
    }
}

/// What an extension set for its button on one page alone (an ad
/// blocker's count of what it blocked there): each `None` is what every
/// page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageValues {
    pub badge: Option<String>,
    pub title: Option<String>,
    pub popup: Option<String>,
    pub enabled: Option<bool>,
}

impl PageValues {
    /// Take in a change for this page; `Some(None)` takes a value back.
    pub fn apply(&mut self, change: &ui::webview::ExtensionActionChange) {
        if let Some(badge) = &change.badge {
            self.badge = badge.as_deref().map(badge_text);
        }
        if let Some(title) = &change.title {
            self.title = title
                .as_deref()
                .map(|title| bounded_title(title).unwrap_or_default());
        }
        if let Some(popup) = &change.popup {
            self.popup.clone_from(popup);
        }
        if let Some(enabled) = change.enabled {
            self.enabled = enabled;
        }
    }
}

fn bounded_title(title: &str) -> Option<String> {
    (!title.is_empty() && title.len() <= MAX_TITLE_BYTES).then(|| title.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub id: String,
    pub label: String,
    pub tooltip: String,
    pub badge: String,
    pub icon: Option<PathBuf>,
    /// The popup a click opens; `None` when a click goes to the extension.
    pub popup: Option<String>,
    /// A button the extension turned off does nothing when clicked.
    pub enabled: bool,
}

impl Button {
    pub fn has_popup(&self) -> bool {
        self.popup.is_some()
    }
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
        // Only among its own pages: the panel shows nothing else.
        let own = format!("chrome-extension://{}/", extension.id);
        let popup = match state.popup {
            Some(popup) => Some(popup).filter(|popup| popup.starts_with(&own)),
            None => extension.popup_url(),
        };
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
            popup,
            enabled: !state.disabled,
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

/// Reads the badge, title, popup and whether the button is on from
/// `chrome.action`, as it is for every tab.
const READ_ACTION: &str = "(async()=>{const a=chrome.action||chrome.browserAction;\
const [text,title,popup,enabled]=await Promise.all([a.getBadgeText({}),a.getTitle({}),\
a.getPopup?a.getPopup({}):null,a.isEnabled?a.isEnabled():true]);\
return JSON.stringify({text,title,popup,enabled})})()";

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
        badge: match state.get("text")?.as_str()? {
            RULE_COUNT_PLACEHOLDER => String::new(),
            text => badge_text(text),
        },
        title: state
            .get("title")
            .and_then(Value::as_str)
            .and_then(bounded_title),
        popup: state
            .get("popup")
            .and_then(Value::as_str)
            .filter(|popup| popup.len() <= 2048)
            .map(str::to_owned),
        disabled: state.get("enabled").and_then(Value::as_bool) == Some(false),
    })
}

/// The extension identifier in a `loadUnpacked` reply.
pub fn loaded_id(body: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(body).ok()?;
    let id = reply.get("id")?.as_str()?;
    crate::crx::is_extension_id(id).then(|| id.to_owned())
}

/// The file at `path` (`/icons/on.png`, from the extension's root) when it
/// is inside `root`: an icon an extension set for its button.
pub fn icon_within(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.strip_prefix('/')?;
    if relative.is_empty()
        || relative.contains(['\\', ':', '?', '#'])
        || relative.split('/').any(|part| part == "..")
    {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let file = root.join(relative).canonicalize().ok()?;
    file.starts_with(&root).then_some(file)
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
            options: options_page(&serde_json::from_str(manifest).unwrap()),
            access: access(&serde_json::from_str(manifest).unwrap()),
            keyword: crate::extension_omnibox::manifest_keyword(
                &serde_json::from_str(manifest).unwrap(),
            ),
        }
    }

    #[test]
    fn an_extension_has_its_host_permissions_apart_from_its_content_scripts_sites() {
        let manifest = serde_json::json!({
            "permissions": ["tabs", "scripting", "https://old.example/*"],
            "host_permissions": ["*://*.example.com/*"],
            "content_scripts": [{"matches": ["<all_urls>"], "js": ["c.js"]}, {"js": ["d.js"]}],
        });
        assert_eq!(
            access(&manifest),
            Access {
                permissions: vec!["tabs".into(), "scripting".into()],
                hosts: vec!["*://*.example.com/*".into(), "https://old.example/*".into()],
                scriptable: vec!["<all_urls>".into()],
                token: None,
                commands: None,
                optional: ExtensionPermissionSet::default(),
                granted: ExtensionPermissionSet::default(),
            }
        );
        assert_eq!(access(&serde_json::json!({})), Access::default());
        // What it may ask for later: MV3 names sites apart, MV2 among the rest.
        let optional = |manifest| access(&manifest).optional;
        assert_eq!(
            optional(serde_json::json!({
                "optional_permissions": ["tabs", "http://old.example/*"],
                "optional_host_permissions": ["https://*.example.org/*"],
            })),
            ExtensionPermissionSet {
                permissions: vec!["tabs".into()],
                origins: vec![
                    "https://*.example.org/*".into(),
                    "http://old.example/*".into()
                ],
            }
        );
        // Its keyboard shortcuts go to the engine as the manifest has them.
        let shortcuts = serde_json::json!({"_execute_action": {"suggested_key": "Alt+Shift+Y"}});
        assert_eq!(
            access(&serde_json::json!({"commands": shortcuts})).commands,
            Some(shortcuts)
        );
        assert_eq!(
            access(&serde_json::json!({"commands": ["toggle"]})).commands,
            None
        );
    }

    #[test]
    fn an_extension_offers_its_settings_page() {
        let extension = installed(
            "abc",
            "uBlock Origin Lite",
            true,
            false,
            r#"{"options_ui":{"page":"dashboard.html","open_in_tab":true}}"#,
        );
        assert_eq!(
            extension.options_url().as_deref(),
            Some("chrome-extension://abc/dashboard.html")
        );
        assert_eq!(
            options_page(&serde_json::json!({"options_page": "options/index.html"})).as_deref(),
            Some("options/index.html")
        );
        for escaping in ["../x.html", "/x.html", "https://example.test/", ""] {
            assert_eq!(
                options_page(&serde_json::json!({"options_page": escaping})),
                None,
                "{escaping}"
            );
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
                ..ActionState::default()
            })
        });
        assert_eq!(model.pinned.len(), 1);
        assert_eq!(model.pinned[0].label, "Vault");
        assert_eq!(model.pinned[0].tooltip, "Vault \u{2014} Open");
        assert!(model.pinned[0].has_popup());
        assert_eq!(model.menu.len(), 2, "disabled and action-less stay out");
        assert!(model.menu[0].pinned && !model.menu[1].pinned);
        assert_eq!(model.menu[1].button.badge, "12");
        assert_eq!(model.menu[1].button.tooltip, "Blocker \u{2014} 12 blocked");
        assert!(!model.menu[1].button.has_popup());
        assert_eq!(
            list[0].popup_url().unwrap(),
            format!("chrome-extension://{}/p.html", "a".repeat(32))
        );
    }

    #[test]
    fn a_page_shows_what_the_extension_set_for_it_over_what_every_page_shows() {
        use ui::webview::ExtensionActionChange as Change;
        let id = "a".repeat(32);
        let own = |page: &str| format!("chrome-extension://{id}/{page}");
        let list = [installed(
            &id,
            "Blocker",
            true,
            true,
            r#"{"action":{"default_popup":"p.html","default_title":"Blocker"}}"#,
        )];
        let mut every = ActionState::default();
        every.apply(&Change {
            badge: Some(Some("on".into())),
            title: Some(Some("Blocking".into())),
            ..Change::default()
        });
        let mut page = PageValues::default();
        page.apply(&Change {
            badge: Some(Some("123456".into())),
            popup: Some(Some(String::new())),
            enabled: Some(Some(false)),
            ..Change::default()
        });
        let button = |state: ActionState| model(&list, |_| Some(state.clone())).pinned.remove(0);
        let shown = button(every.clone().under(&page));
        assert_eq!(shown.badge, "1234", "Chrome's four characters");
        assert_eq!(shown.tooltip, "Blocker \u{2014} Blocking");
        assert_eq!(shown.popup, None, "a click goes to the extension");
        assert!(!shown.enabled);

        // Taken back for the page: what every page shows.
        page.apply(&Change {
            badge: Some(None),
            popup: Some(None),
            enabled: Some(None),
            ..Change::default()
        });
        let shown = button(every.clone().under(&page));
        assert_eq!(shown.badge, "on");
        assert_eq!(shown.popup, Some(own("p.html")), "the manifest's popup");
        assert!(shown.enabled);

        // A popup Chromium resolved stands, but only among its own pages.
        every.apply(&Change {
            popup: Some(Some(own("other.html"))),
            ..Change::default()
        });
        assert_eq!(button(every.clone()).popup, Some(own("other.html")));
        every.popup = Some("https://example.test/".into());
        assert_eq!(button(every.clone()).popup, None);
        // An empty title for the page is no title.
        page.apply(&Change {
            title: Some(Some(String::new())),
            ..Change::default()
        });
        assert_eq!(
            button(ActionState::default().under(&page)).tooltip,
            "Blocker"
        );
    }

    #[test]
    fn an_icon_an_extension_sets_is_one_of_its_own_files() {
        let root = std::env::temp_dir().join(format!("tilo-action-icon-{}", std::process::id()));
        std::fs::create_dir_all(root.join("icons")).unwrap();
        std::fs::write(root.join("icons/on.png"), b"png").unwrap();
        assert_eq!(
            icon_within(&root, "/icons/on.png"),
            Some(root.canonicalize().unwrap().join("icons/on.png"))
        );
        for outside in [
            "icons/on.png",
            "/../on.png",
            "/icons/../../on.png",
            "/",
            "/missing.png",
        ] {
            assert_eq!(icon_within(&root, outside), None, "{outside}");
        }
        std::fs::remove_dir_all(&root).unwrap();
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
                title: Some("6 blocked".into()),
                ..ActionState::default()
            })
        );
        let off = json!({"result":{"type":"string","value":
            json!({"text":"","title":"","popup":format!("chrome-extension://{id}/p.html"),"enabled":false}).to_string()}})
        .to_string();
        assert_eq!(
            action_state(&off),
            Some(ActionState {
                popup: Some(format!("chrome-extension://{id}/p.html")),
                disabled: true,
                ..ActionState::default()
            })
        );
        // A button that shows each page's count has no text for every tab.
        let counting = json!({"result":{"type":"string","value":
            json!({"text":RULE_COUNT_PLACEHOLDER,"title":"uBlock Origin Lite"}).to_string()}})
        .to_string();
        assert_eq!(
            action_state(&counting),
            Some(ActionState {
                title: Some("uBlock Origin Lite".into()),
                ..ActionState::default()
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
