//! What this Tilo is, said the same way wherever Tilo says it: that it is
//! Tilo, an operating system, where it runs, which version, and where to go
//! next (tilo.so, to get it; be told when it comes to your computer, its
//! source, its community).
//!
//! [`AboutVariant`] picks the words for where Tilo runs, [`AboutLinks`] is
//! the deployment's set of links (one left empty is not offered) and
//! [`about_view`] lays them out. [`TiloMenu`] is the small menu behind the
//! top bar's Tilo mark ([`crate::topbar::TopBarChrome::with_tilo_mark`])
//! and the About sheet it opens; a host shows it under the mark when the
//! bar asks with [`crate::chrome::ChromeIntent::ToggleTiloMenu`].

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use ui::TextRole;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    Column, ContextMenu, Link, MenuItem, ModalDialog, Row, TaffyTreeExt as _, Text, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};

/// Tilo's home on the web: where to read about it and get it.
pub const TILO_HOME: &str = "https://tilo.so";

/// Where the Tilo being described runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AboutVariant {
    /// Installed on this computer.
    Native,
    /// In a browser tab, ahead of running on computers natively.
    Browser,
}

/// The deployment's links. Each is an `http(s)` URL or absent; anything else
/// (an empty setting included) is not offered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AboutLinks {
    /// Where to be told when Tilo comes to computers.
    pub notify: Option<String>,
    /// Tilo's source code.
    pub source: Option<String>,
    /// Where the people who use and build Tilo talk.
    pub community: Option<String>,
}

impl AboutLinks {
    /// The links from their settings; an empty or non-web setting is none.
    pub fn new(notify: &str, source: &str, community: &str) -> Self {
        fn web(url: &str) -> Option<String> {
            let url = url.trim();
            (url.starts_with("https://") || url.starts_with("http://"))
                .then(|| url.to_owned())
                .filter(|url| !url.contains(char::is_whitespace))
        }
        Self {
            notify: web(notify),
            source: web(source),
            community: web(community),
        }
    }

    /// The links offered, in reading order, as `(label, url)`.
    pub fn shown(&self) -> Vec<(String, String)> {
        [
            ("tilo-about-link-notify", &self.notify),
            ("tilo-about-link-source", &self.source),
            ("tilo-about-link-community", &self.community),
        ]
        .into_iter()
        .filter_map(|(key, url)| url.as_ref().map(|url| (t(key), url.clone())))
        .collect()
    }
}

/// Read from a host's settings as `{notify, source, community}`, each a URL
/// or empty; a missing one is empty.
impl<'de> serde::Deserialize<'de> for AboutLinks {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Settings {
            notify: String,
            source: String,
            community: String,
        }
        let settings = Settings::deserialize(deserializer)?;
        Ok(Self::new(
            &settings.notify,
            &settings.source,
            &settings.community,
        ))
    }
}

/// The About sheet's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutCopy {
    /// The sheet's title, and the menu entry that opens it.
    pub title: String,
    pub menu: String,
    /// That this is Tilo, an operating system.
    pub lead: String,
    /// Where it runs.
    pub body: String,
    /// Which Tilo this is.
    pub version: String,
    /// Tilo's home on the web ([`TILO_HOME`]): in a browser, where to get
    /// Tilo; on a computer that has it, just its address.
    pub home: String,
}

/// The words for Tilo running as `variant`. A browser tab is not "this
/// computer": there it is About Tilo.
pub fn about_copy(variant: AboutVariant) -> AboutCopy {
    let (title, menu, body, home) = match variant {
        AboutVariant::Native => (
            "tilo-about-title",
            "tilo-menu-about",
            "tilo-about-body-native",
            "tilo-about-link-home",
        ),
        AboutVariant::Browser => (
            "tilo-about-title-browser",
            "tilo-menu-about-browser",
            "tilo-about-body-browser",
            "tilo-about-link-get",
        ),
    };
    AboutCopy {
        title: t(title),
        menu: t(menu),
        lead: t("tilo-about-lead"),
        body: t(body),
        version: t_args(
            "tilo-about-version",
            &[("version", env!("CARGO_PKG_VERSION").into())],
        ),
        home: t(home),
    }
}

/// The Tilo mark at the head of the sheet: no control, just the mark.
struct Mark {
    rect: Rect,
}

impl Widget for Mark {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let side = theme.tokens.metrics.icon_md * 1.5;
        let style = Style {
            size: taffy::Size::from_lengths(side, side),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        crate::topbar::paint_tilo_mark(scene, transform, self.rect, theme);
    }

    fn name(&self) -> &'static str {
        "TiloMark"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The About sheet's body: the mark, the words for `variant` and the links
/// offered.
pub fn about_view(variant: AboutVariant, links: &AboutLinks) -> Column {
    let copy = about_copy(variant);
    let gap = ui::tokens::Spacing::default().s3;
    let column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(gap)
        .push(Box::new(Mark { rect: Rect::ZERO }))
        .push(Box::new(
            Text::new(&copy.lead).with_role(TextRole::Subheading),
        ))
        .push(Box::new(Text::new(&copy.body)))
        .push(Box::new(
            Text::new(&copy.version).with_role(TextRole::Caption),
        ));
    // Tilo's home leads, offered everywhere: the deployment's own links
    // follow.
    let mut row = Row::new()
        .with_gap(ui::tokens::Spacing::default().s4)
        .push(Box::new(Link::new(TILO_HOME).with_label(&copy.home)));
    for (label, url) in links.shown() {
        row = row.push(Box::new(Link::new(&url).with_label(&label)));
    }
    column.push(Box::new(row))
}

/// The menu behind the bar's Tilo mark and the About sheet it opens.
pub struct TiloMenu {
    menu: ContextMenu,
    about: ModalDialog<Column>,
    /// Set by the sheet's close control, Escape or a click outside it.
    dismissed: Rc<Cell<bool>>,
    /// The menu offers Feedback: the host has a destination for it.
    offers_feedback: bool,
    /// Feedback was chosen and the host has not yet opened it.
    feedback: bool,
    /// The About entry's label for where Tilo runs.
    about_label: String,
    rect: Rect,
}

impl TiloMenu {
    pub fn new(variant: AboutVariant, links: &AboutLinks) -> Self {
        let variant_menu = about_copy(variant).menu;
        let dismissed = Rc::new(Cell::new(false));
        let dismiss = Rc::clone(&dismissed);
        let copy = about_copy(variant);
        Self {
            menu: ContextMenu::with_items(vec![MenuItem::action(copy.menu.clone(), || {})]),
            about: ModalDialog::new(copy.title, about_view(variant, links))
                .with_test_id("tilo-about")
                .with_max_width(440.0)
                .on_dismiss(move || dismiss.set(true)),
            dismissed,
            offers_feedback: false,
            feedback: false,
            about_label: variant_menu,
            rect: Rect::ZERO,
        }
    }

    /// Offer Feedback as the menu's second entry, for a host that has
    /// somewhere to send it.
    #[must_use]
    pub fn with_feedback(mut self) -> Self {
        self.offer_feedback();
        self
    }

    /// [`Self::with_feedback`] for a menu already shown, once the host has
    /// somewhere to send it.
    pub fn offer_feedback(&mut self) {
        self.offers_feedback = true;
        self.menu.set_items(vec![
            MenuItem::action(self.about_label.clone(), || {}),
            MenuItem::action(t("tilo-menu-feedback"), || {}),
        ]);
    }

    /// Whether Feedback was chosen since the last call; the host opens it
    /// about the window in front.
    pub fn take_feedback(&mut self) -> bool {
        std::mem::take(&mut self.feedback)
    }

    /// Open the menu under `anchor` (the mark) within `viewport`, or close
    /// it when it is open.
    pub fn toggle(&mut self, anchor: Rect, viewport: Rect) {
        if self.menu.is_open() {
            self.menu.close();
        } else {
            self.menu.open_anchored(anchor, viewport);
            self.menu.focus_first_action();
        }
    }

    /// Show the About sheet (the menu's first entry). The menu goes at
    /// once: fading out under the sheet's scrim it showed through it, over
    /// what the scrim dims.
    pub fn show_about(&mut self) {
        self.menu.close_at_once();
        self.about.set_open(true);
    }

    pub fn is_menu_open(&self) -> bool {
        self.menu.is_open()
    }

    pub fn is_about_open(&self) -> bool {
        self.about.is_open()
    }

    /// The menu or the sheet is up.
    pub fn is_open(&self) -> bool {
        self.is_menu_open() || self.is_about_open()
    }

    /// Apply what the person chose since the last call; true when anything
    /// changed.
    pub fn settle(&mut self) -> bool {
        let mut changed = false;
        match self.menu.take_selection() {
            Some(0) => {
                self.show_about();
                changed = true;
            }
            Some(1) if self.offers_feedback => {
                self.menu.close();
                self.feedback = true;
                changed = true;
            }
            _ => {}
        }
        if self.dismissed.replace(false) {
            self.about.set_open(false);
            changed = true;
        }
        changed
    }
}

impl Widget for TiloMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let menu = self.menu.build_layout(taffy, theme, map);
        let about = self.about.build_layout(taffy, theme, map);
        // Over the whole host, out of its flow: the sheet's scrim covers it.
        let style = Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(0.0),
                right: taffy::LengthPercentageAuto::length(0.0),
                top: taffy::LengthPercentageAuto::length(0.0),
                bottom: taffy::LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[menu, about], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [menu, about] = taffy.children(node).unwrap_or_default()[..] {
            self.menu.apply_layout(taffy, menu, inner);
            self.about.apply_layout(taffy, about, inner);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [menu, about] = nodes[..] else {
            return;
        };
        if self.about.is_open() {
            self.about
                .handle_event(event, &mut ctx.reborrow(ctx.path, about));
        } else if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        }
        self.settle();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.menu, &self.about]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.menu, &mut self.about]
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    fn name(&self) -> &'static str {
        "TiloMenu"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_non_web_links_are_not_offered() {
        ui::i18n::I18nManager::init("en-US");
        let links = AboutLinks::new(" https://example.org/notify ", "", "javascript:alert(1)");
        assert_eq!(links.notify.as_deref(), Some("https://example.org/notify"));
        assert_eq!(links.source, None);
        assert_eq!(links.community, None);
        assert_eq!(
            links.shown(),
            vec![(
                t("tilo-about-link-notify"),
                "https://example.org/notify".to_owned()
            )]
        );
        assert!(AboutLinks::new("", "", "").shown().is_empty());
    }

    #[test]
    fn settings_with_empty_or_missing_links_offer_only_the_rest() {
        let links: AboutLinks =
            serde_json::from_str(r#"{"notify":"","source":"https://example.org/src"}"#)
                .expect("about settings");
        assert_eq!(
            links,
            AboutLinks {
                notify: None,
                source: Some("https://example.org/src".to_owned()),
                community: None,
            }
        );
        let none: AboutLinks = serde_json::from_str("{}").expect("no about settings");
        assert_eq!(none, AboutLinks::default());
    }

    #[test]
    fn the_browser_says_it_runs_in_the_browser() {
        ui::i18n::I18nManager::init("en-US");
        let browser = about_copy(AboutVariant::Browser);
        let native = about_copy(AboutVariant::Native);
        assert_eq!(browser.lead, native.lead);
        assert!(browser.lead.contains("Tilo"), "{}", browser.lead);
        assert!(browser.body.contains("browser"), "{}", browser.body);
        assert!(!native.body.contains("browser"), "{}", native.body);
        // A browser tab is not "this computer".
        for words in [&browser.title, &browser.menu, &browser.body] {
            assert!(
                !words.contains("computer") || words.contains("own computer"),
                "{words}"
            );
        }
        assert!(browser.title.contains("Tilo"), "{}", browser.title);
        assert!(native.title.contains("computer"), "{}", native.title);
        // Both say which Tilo this is, and where to find it on the web; the
        // browser says that is where to get it.
        for copy in [&browser, &native] {
            assert!(
                copy.version.contains(env!("CARGO_PKG_VERSION")),
                "{}",
                copy.version
            );
        }
        assert!(browser.home.contains("Get"), "{}", browser.home);
        assert_eq!(native.home, "tilo.so");
    }

    /// Choosing About takes the menu away at once: a menu still fading out
    /// showed through the sheet's scrim, over the window it dims.
    #[test]
    fn about_takes_the_menu_away_at_once() {
        ui::i18n::I18nManager::init("en-US");
        let mut menu = TiloMenu::new(AboutVariant::Browser, &AboutLinks::default());
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        ui::time::testing::with_monotonic_time(ui::time::now(), || {
            menu.toggle(Rect::new(8.0, 4.0, 40.0, 36.0), viewport);
            assert!(menu.menu.is_drawn());
            menu.menu.click_item(0);
            assert!(menu.settle());
            assert!(menu.is_about_open());
            assert!(
                !menu.menu.is_drawn(),
                "nothing of the menu paints under the sheet"
            );
        });
    }

    #[test]
    fn the_menu_opens_about_and_the_sheet_closes_on_dismiss() {
        ui::i18n::I18nManager::init("en-US");
        let mut menu = TiloMenu::new(AboutVariant::Browser, &AboutLinks::default());
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let anchor = Rect::new(8.0, 4.0, 40.0, 36.0);
        menu.toggle(anchor, viewport);
        assert!(menu.is_menu_open());
        menu.toggle(anchor, viewport);
        assert!(!menu.is_open());
        menu.toggle(anchor, viewport);
        menu.menu.click_item(0);
        assert!(menu.settle());
        assert!(menu.is_about_open() && !menu.is_menu_open());
        menu.dismissed.set(true);
        assert!(menu.settle());
        assert!(!menu.is_open());
        assert!(
            !menu.take_feedback(),
            "a menu without feedback never asks for it"
        );
    }

    #[test]
    fn choosing_feedback_closes_the_menu_and_asks_the_host_once() {
        ui::i18n::I18nManager::init("en-US");
        let mut menu = TiloMenu::new(AboutVariant::Browser, &AboutLinks::default()).with_feedback();
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        menu.toggle(Rect::new(8.0, 4.0, 40.0, 36.0), viewport);
        menu.menu.click_item(1);
        assert!(menu.settle());
        assert!(
            !menu.is_open(),
            "feedback opens as a window, not over the menu"
        );
        assert!(menu.take_feedback());
        assert!(!menu.take_feedback());
    }
}
