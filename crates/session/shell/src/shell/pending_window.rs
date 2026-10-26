//! The place a window will open in, held from the moment it is asked for.
//!
//! A launch can take seconds before the app has a window (a cold start, an
//! install first), and the tab only becomes the app once that window maps.
//! Until then the tab holds this content. It draws nothing of its own and
//! never presents: what stands in the window's place is the framework's
//! arrival of its first frame ([`ui::arriving`]): the place reserved at the
//! window's size, a skeleton after the grace period, and the app's first
//! frame cross-fading in once its window maps in this tab (the tile goes on
//! with the same arrival). An app fetched from its registry first, to be
//! installed or updated, comes from that host: the placeholder names it
//! after a while and says how much of it has arrived. A failure or a missed
//! deadline turns the tab into the retry card in the same place
//! (`peer_windows::settle_local_restore`).

use std::cell::Cell;
use std::collections::HashMap;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use ui::Theme;
use ui::arriving::{Progress, Source};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{AppKind, AppWidget, IconDescriptor, TaffyTreeExt, Widget};

use crate::shell::catalogue::{self, InstallProgress};

/// What the place is waiting for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The app to start, on this device.
    Start,
    /// The app to be fetched from its registry and installed first.
    Install { package_id: String },
}

pub struct PendingWindow {
    /// The tab holding the place, whose window is configured to its size
    /// the moment it appears (`ForeignFrameStore::bind_opening_tab`).
    tab: Option<crate::shell::state::TabId>,
    title: String,
    wait: Wait,
    rect: Rect,
    /// The package whose window this is, when it is known: while that
    /// package is updated before it starts (`crate::shell::app_versions`),
    /// its download is what the window waits on.
    package: Option<String>,
    /// The last download measured, which stands full once the bytes are in
    /// and the app installs.
    measured: Cell<Option<Progress>>,
}

impl PendingWindow {
    pub fn new(title: &str, wait: Wait) -> Self {
        Self {
            tab: None,
            title: title.to_owned(),
            wait,
            rect: Rect::ZERO,
            package: None,
            measured: Cell::new(None),
        }
    }

    #[must_use]
    pub fn for_package(mut self, package: &str) -> Self {
        self.package = Some(package.to_owned());
        self
    }

    /// The place of `tab`, publishing where it is laid out for the window
    /// that will take it.
    #[must_use]
    pub fn for_tab(mut self, tab: Option<crate::shell::state::TabId>) -> Self {
        self.tab = tab;
        self
    }

    /// The package being downloaded before the window can open, if any: an
    /// install, or an update of the app before it starts.
    pub(crate) fn downloading(&self) -> Option<&str> {
        if let Wait::Install { package_id } = &self.wait {
            return Some(package_id);
        }
        self.package
            .as_deref()
            .filter(|package| crate::shell::app_versions::updating(package))
    }
}

/// The host a package is fetched from, as its registry's address names it.
fn registry_host(package_id: &str) -> Option<String> {
    let (_, apps) = catalogue::available_apps_snapshot();
    let app = apps.iter().find(|app| app.package_id == package_id)?;
    let after_scheme = app
        .download_url
        .split_once("://")
        .map_or(app.download_url.as_str(), |(_, rest)| rest);
    let host = after_scheme.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    (!host.is_empty()).then(|| host.to_owned())
}

impl Widget for PendingWindow {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: Size::from_percent(1.0, 1.0),
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(tab) = self.tab {
            crate::shell::foreign::store().note_opening_tab(tab, self.rect);
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Nothing: the window's arrival stands in this place.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn name(&self) -> &'static str {
        "PendingWindow"
    }

    /// Never: the window it stands for is another content, which takes the
    /// tab over when it maps.
    fn presented(&self) -> bool {
        false
    }

    fn arrives_from(&self) -> Source {
        self.downloading()
            .and_then(registry_host)
            .map_or(Source::Local, Source::host)
    }

    fn arrival_progress(&self) -> Option<Progress> {
        let package = self.downloading()?;
        let measured = match catalogue::install_progress(package) {
            Some(InstallProgress::Downloading { received, total }) => {
                Some(Progress::new(received, total))
            }
            // The bytes are in and the app installs: what arrived stands
            // full until the window does.
            Some(InstallProgress::Installing) => self
                .measured
                .get()
                .map(|last| Progress::new(last.total.unwrap_or(last.received), last.total)),
            None => self.measured.get(),
        };
        self.measured.set(measured);
        measured
    }
}

impl AppWidget for PendingWindow {
    fn uses_host_record(&self) -> bool {
        false
    }

    fn app_kind(&self) -> AppKind {
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰏗")
    }
}

#[cfg(test)]
#[path = "pending_window_tests.rs"]
mod tests;
