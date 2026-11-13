//! The view bound to each swappable part of the shell: the person's setting
//! (`settings:shell-views`), kept wherever the session runs the shell, so an
//! agent or a script binds a slot the way a person does.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use settings_store::device_class::DeviceClass;
use ui::tilo_resource;

/// The parts of the shell a person may bind a view to.
pub const SWAPPABLE: &[&str] = &[
    "wm",
    "window-decoration",
    "bar",
    "quick-settings",
    "launcher",
    "switcher",
    "overview",
    "strips",
    "desk-widgets",
    "notifications",
    "people",
    "emoji",
    "osd",
    "window-menu",
    "workspace-menu",
];

/// The parts the system draws itself, whatever is bound to them.
pub const FIXED: &[&str] = &["lock", "consent", "portal", "auth", "display-confirm"];

/// The prefix of a sheet's slot: `sheet:power`, `sheet:display`.
pub const SHEET: &str = "sheet:";

/// The view bound to a slot.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct Binding {
    /// The app whose view draws the slot.
    pub app: String,
    /// The view it publishes, by the id it is published under.
    pub view: String,
}

/// The view bound to each slot: everywhere the person works, and on one kind
/// of device where that differs.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
pub struct ShellViews {
    /// The view each slot shows, by the slot's name.
    #[serde(default)]
    #[model(readonly)]
    pub bindings: BTreeMap<String, Binding>,
    /// The views bound on one kind of device, over the ones bound everywhere:
    #[serde(default)]
    #[model(readonly)]
    pub on_device: BTreeMap<String, BTreeMap<String, Binding>>,
}

impl ui::storage::SettingsSchema for ShellViews {
    const SCOPE: ui::storage::Scope =
        ui::storage::Scope::Shared(settings_store::shell_views::SECTION);
    ui::settings_codec!();
}

#[ui::tilo_resource::resource(kind = "shell-views")]
impl ShellViews {
    /// Bind `slot` to the view `view` of `app`, on every device or on the
    /// kind of device `on` names.
    #[action(id = "bind")]
    pub fn bind(
        &mut self,
        #[param("The part of the shell: `window-menu`, `bar`, `sheet:power`, …")] slot: String,
        #[param("The app whose view draws it")] app: String,
        #[param("The view the app publishes")] view: String,
        #[param("The kind of device it is for (`native`, `web`); every device by default")]
        on: Option<String>,
    ) -> Result<(), tilo_resource::Refusal> {
        let slot = slot_named(&slot)?;
        let app = app.trim();
        let view = view.trim();
        if app.is_empty() {
            return Err(tilo_resource::Refusal::new(
                "a view is an app's: name the app",
            ));
        }
        if view.is_empty() {
            return Err(tilo_resource::Refusal::new(
                "the app publishes its views: name the one to draw the slot",
            ));
        }
        let binding = Binding {
            app: app.to_owned(),
            view: view.to_owned(),
        };
        match on.as_deref() {
            Some(class) => self
                .on_device
                .entry(device_named(class)?)
                .or_default()
                .insert(slot, binding),
            None => self.bindings.insert(slot, binding),
        };
        Ok(())
    }

    /// Show `slot` the way the shell ships it again, on every device or on
    /// one kind of device.
    #[action(id = "reset")]
    pub fn reset(
        &mut self,
        #[param("The part of the shell to show as the shell ships it")] slot: String,
        #[param("The kind of device it is for (`native`, `web`); every device by default")]
        on: Option<String>,
    ) -> Result<(), tilo_resource::Refusal> {
        let slot = slot_named(&slot)?;
        match on.as_deref() {
            Some(class) => {
                if let Some(bindings) = self.on_device.get_mut(&device_named(class)?) {
                    bindings.remove(&slot);
                }
            }
            None => {
                self.bindings.remove(&slot);
            }
        }
        Ok(())
    }
}

impl ShellViews {
    /// The view bound to `slot` on `device`: the one bound for that kind of
    /// device, else the one bound everywhere.
    #[must_use]
    pub fn bound(&self, slot: &str, device: DeviceClass) -> Option<&Binding> {
        self.on_device
            .get(settings_store::shell_views::device_key(device))
            .and_then(|bindings| bindings.get(slot))
            .or_else(|| self.bindings.get(slot))
    }
}

/// `slot`, as the shell names the part it is.
fn slot_named(slot: &str) -> Result<String, tilo_resource::Refusal> {
    let slot = slot.trim();
    let sheet = slot.strip_prefix(SHEET);
    let kind = sheet.unwrap_or(slot);
    if FIXED.contains(&kind) {
        return Err(tilo_resource::Refusal::new(format!(
            "the shell draws {kind} itself: it asks the person something on the machine's \
             behalf, and only the system's own view may"
        )));
    }
    if sheet.is_some() {
        if kind.is_empty() {
            return Err(tilo_resource::Refusal::new("a sheet is named by its kind"));
        }
        return Ok(slot.to_owned());
    }
    SWAPPABLE
        .contains(&kind)
        .then(|| kind.to_owned())
        .ok_or_else(|| {
            tilo_resource::Refusal::new(format!(
                "the shell has no part `{kind}`: it shows {}",
                SWAPPABLE.join(", ")
            ))
        })
}

/// The kind of device `text` names.
fn device_named(text: &str) -> Result<String, tilo_resource::Refusal> {
    let text = text.trim();
    match text {
        "native" | "web" => Ok(text.to_owned()),
        other => Err(tilo_resource::Refusal::new(format!(
            "`{other}` is not a kind of device: `native` or `web`"
        ))),
    }
}
