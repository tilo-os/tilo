//! An extension's side panel (`chrome.sidePanel`) beside the window's page:
//! what the extension asks of it, and the window's controls that show and
//! close it.

use ui::icons::Icon;
use ui::webview::{ExtensionSidePanelChange, SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::{Button, Container, Element, MenuItem};
use ui::window_toolbar::ToolbarAction;

use super::{BAR_SIDE_PANEL, Command, ExtensionController, Panel};
use crate::CATALOG;
use crate::effects::ExtensionRequest;
use crate::extension_toolbar::Installed;

const SIDE_PANEL_BUTTON: &str = "browser.extension.side-panel";
/// The bar's button closing the side panel that shows.
const BAR_CLOSE_SIDE_PANEL: &str = "extension-side-panel-close";

/// An extension's own page beside the window's page (`chrome.sidePanel`):
/// no tab of its own, and that page is its window.
pub(super) struct SidePanel {
    pub(super) id: String,
    pub(super) url: String,
    pub(super) engine: SharedEngine,
}

/// What an extension's side panel is called: "Notes side panel".
fn side_panel_name(name: &str) -> String {
    CATALOG.t_args(
        "browser-extension-side-panel",
        &[("name", ui::i18n::ArgValue::Text(name.to_owned()))],
    )
}

/// The bar's items showing or hiding each side panel, from
/// [`ExtensionController::side_panel_entries`].
pub(super) fn bar_items(
    entries: Vec<(String, String, bool)>,
) -> impl Iterator<Item = ToolbarAction> {
    entries.into_iter().map(|(id, name, shown)| {
        ToolbarAction::new(
            format!("{BAR_SIDE_PANEL}{id}"),
            "panel-right",
            side_panel_name(&name),
        )
        .on(shown)
    })
}

/// What a script's or an agent's request to open or close the side panel
/// of `extension` does.
pub(super) fn command(
    request: &ExtensionRequest,
    extension: &Installed,
) -> Result<Command, String> {
    if !extension.enabled || !extension.side_panel() {
        return Err("This extension is off or has no side panel".into());
    }
    Ok(match request {
        ExtensionRequest::CloseSidePanel(_) => Command::CloseSidePanel(extension.id.clone()),
        _ => Command::OpenSidePanel(extension.id.clone()),
    })
}

impl ExtensionController {
    /// What an extension asked of its side panel beside this window's page.
    /// Returns whether the window's layout changed.
    pub(super) fn side_panel_changed(
        &mut self,
        id: String,
        change: ExtensionSidePanelChange,
        engine: &SharedEngine,
    ) -> bool {
        let shows = self.side_panel.as_ref().is_some_and(|panel| panel.id == id);
        let url = match change {
            ExtensionSidePanelChange::Open(url) => url,
            ExtensionSidePanelChange::Toggle(url) if !shows => url,
            ExtensionSidePanelChange::Options(Some(url)) if shows => url,
            ExtensionSidePanelChange::Toggle(_)
            | ExtensionSidePanelChange::Options(None)
            | ExtensionSidePanelChange::Close
                if shows =>
            {
                self.side_panel = None;
                return true;
            }
            ExtensionSidePanelChange::Toggle(_)
            | ExtensionSidePanelChange::Options(_)
            | ExtensionSidePanelChange::Close => return false,
        };
        if !self.running(&id) {
            return false;
        }
        // Its popup gave way to it, as a popup closes in Chrome once the
        // person turns to the page or the panel.
        if matches!(&self.panel, Some(Panel::Popup { id: open, .. }) if *open == id) {
            self.panel = None;
        }
        if let Some(panel) = self.side_panel.as_mut().filter(|panel| panel.id == id) {
            if panel.url != url {
                panel.engine.send(WebViewCommand::Navigate(url.clone()));
                panel.url = url;
            }
            return false;
        }
        match engine.create_tab() {
            Ok(page) => {
                // Its "current tab" is the page beside it.
                page.send(WebViewCommand::ExtensionPanel { action: None });
                page.send(WebViewCommand::Navigate(url.clone()));
                self.side_panel = Some(SidePanel {
                    id,
                    url,
                    engine: page,
                });
            }
            Err(error) => self.panel = Some(Panel::Message(error)),
        }
        true
    }

    /// Whether the extension `id` is installed and on.
    pub(super) fn running(&self, id: &str) -> bool {
        self.installed
            .iter()
            .any(|extension| extension.id == id && extension.enabled)
    }

    /// Whether the extension `id` may have a side panel and is on.
    pub(super) fn has_side_panel(&self, id: &str) -> bool {
        self.installed
            .iter()
            .any(|extension| extension.id == id && extension.enabled && extension.side_panel())
    }

    /// The extension whose side panel shows beside this window's page.
    pub fn side_panel_shown(&self) -> Option<&str> {
        self.side_panel.as_ref().map(|panel| panel.id.as_str())
    }

    /// The bar's button closing the side panel that shows, as a slot of the
    /// window's bar in a Tilo session.
    pub fn bar_side_panel(&self) -> Option<ToolbarAction> {
        self.side_panel.as_ref()?;
        Some(
            ToolbarAction::new(
                BAR_CLOSE_SIDE_PANEL,
                "panel-right",
                CATALOG.t("browser-extension-side-panel-close"),
            )
            .on(true),
        )
    }

    /// The side panel beside the window's page, `width` wide: the
    /// extension's page, which the person uses beside the page it is for.
    pub fn side_panel(&self, width: f32) -> Option<Element> {
        let panel = self.side_panel.as_ref()?;
        let name = self
            .installed
            .iter()
            .find(|extension| extension.id == panel.id)
            .map_or("Extension", |extension| extension.name.as_str());
        let view = ui::widget::WebView::new(panel.engine.clone())
            .with_accessible_name(side_panel_name(name))
            .with_test_id("browser.extension.side-panel");
        Some(Box::new(
            Container::new()
                .with_no_padding()
                .with_width(width)
                .filling()
                .with_child(Box::new(view)),
        ))
    }

    /// The extensions that may show a side panel, each with its name and
    /// whether its panel shows beside this page.
    pub(super) fn side_panel_entries(&self) -> Vec<(String, String, bool)> {
        self.installed
            .iter()
            .filter(|extension| extension.enabled && extension.side_panel())
            .map(|extension| {
                let shown = self.side_panel_shown() == Some(extension.id.as_str());
                (extension.id.clone(), extension.name.clone(), shown)
            })
            .collect()
    }

    /// Show the side panel of `id` beside the page, or close it where it
    /// shows.
    pub(super) fn toggle_side_panel(&self, id: &str) -> Command {
        if self.side_panel_shown() == Some(id) {
            Command::CloseSidePanel(id.to_owned())
        } else {
            Command::OpenSidePanel(id.to_owned())
        }
    }

    /// What the side panel's page did since the last frame: closed itself
    /// (`window.close()`), asked for more access (`permissions.request`,
    /// which the host puts to the page that asks), or opened pages, which
    /// open beside the window as from a popup. Returns whether the window
    /// changed.
    pub(super) fn sync_side_panel(&mut self, engine: &SharedEngine) -> bool {
        let Some(panel) = &self.side_panel else {
            return false;
        };
        let mut closed = false;
        let mut asked = Vec::new();
        for event in panel.engine.drain_events() {
            match event {
                WebViewEvent::TabClosed => closed = true,
                // Its question is the window's to ask, as a popup's is.
                event @ (WebViewEvent::ExtensionPermissionRequest { .. }
                | WebViewEvent::ExtensionPermissions { .. }) => asked.push(event),
                _ => {}
            }
        }
        super::open_beside(&panel.engine);
        if closed {
            self.side_panel = None;
        }
        let mut changed = closed;
        for event in asked {
            changed |= self.on_engine_event(event, engine);
        }
        changed
    }

    /// The side panel of an extension turned off or removed closes with it.
    pub(super) fn keep_side_panel(&mut self, installed: &[Installed]) {
        if let Some(panel) = &self.side_panel
            && !installed
                .iter()
                .any(|extension| extension.id == panel.id && extension.enabled)
        {
            self.side_panel = None;
        }
    }

    /// Ask the extension which of its pages its panel shows beside this
    /// page; the engine answers with it.
    pub(super) fn open_side_panel(&self, id: String, engine: &SharedEngine) {
        if self.has_side_panel(&id) {
            engine.send(WebViewCommand::ExtensionOpenSidePanel { id });
        }
    }

    pub(super) fn close_side_panel(&mut self, id: &str) {
        if self.side_panel_shown() == Some(id) {
            self.side_panel = None;
        }
    }

    /// The toolbar row's button closing the side panel that shows.
    pub(super) fn side_panel_button(&self) -> Option<Element> {
        let panel = self.side_panel.as_ref()?;
        let close = CATALOG.t("browser-extension-side-panel-close");
        Some(Box::new(
            Button::icon_only(SIDE_PANEL_BUTTON, Icon::PanelRight, &close)
                .tooltip(&close)
                .on_effect(self.queue(Command::CloseSidePanel(panel.id.clone()))),
        ))
    }

    /// The Extensions menu's items showing or hiding each side panel.
    pub(super) fn side_panel_menu_items(&self) -> Vec<MenuItem> {
        self.side_panel_entries()
            .into_iter()
            .map(|(id, name, shown)| {
                let toggle = std::cell::RefCell::new(self.queue(self.toggle_side_panel(&id)));
                MenuItem::action(side_panel_name(&name), move || (toggle.borrow_mut())())
                    .with_checked(shown)
            })
            .collect()
    }

    /// The bar action `id` when it shows or hides a side panel.
    pub(super) fn side_panel_bar_command(&self, id: &str) -> Option<Command> {
        if let Some(extension) = id.strip_prefix(BAR_SIDE_PANEL) {
            return Some(self.toggle_side_panel(extension));
        }
        (id == BAR_CLOSE_SIDE_PANEL).then(|| {
            let shown = self.side_panel_shown().unwrap_or_default();
            Command::CloseSidePanel(shown.to_owned())
        })
    }

    /// The extension's resource action opening its side panel beside this
    /// page, or closing it where it shows.
    pub(super) fn side_panel_action(
        &self,
        extension: &Installed,
    ) -> Option<ui::tilo_actions::ActionSpec> {
        use crate::effects::BrowserEffect as E;
        use ui::tilo_resource::Effect as _;
        if !extension.enabled || !extension.side_panel() {
            return None;
        }
        let id = extension.id.clone();
        Some(if self.side_panel_shown() == Some(id.as_str()) {
            E::CloseExtensionSidePanel { id }.bound()
        } else {
            E::OpenExtensionSidePanel { id }.bound()
        })
    }
}
