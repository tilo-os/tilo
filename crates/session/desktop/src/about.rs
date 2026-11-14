//! What this Tilo is, said the same way wherever Tilo says it: that it is Tilo, an operating
//! system, where it runs, which version, and where to go next (tilo.so, to get it; be told when it
//! comes to your computer, its source, its community).
//! [`AboutVariant`] picks the words for where Tilo runs, [`AboutLinks`] is the deployment's set of
//! links (one left empty is not offered) and [`about_view`] lays them out. [`AboutSheet`] shows
//! them over the window the person asked from, opened by the chrome's `about` verb.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use ui::TextRole;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Column, Link, ModalDialog, Row, TaffyTreeExt as _, Text, Widget};
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

/// The deployment's links. Each is an `http(s)` URL or absent; anything else (an empty setting
/// included) is not offered.
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

    /// The links offered, in reading order, as `(label, url)` .
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

/// Read from a host's settings as `{notify, source, community}` , each a URL or empty; a missing
/// one is empty.
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
    /// The sheet's title.
    pub title: String,
    /// That this is Tilo, an operating system.
    pub lead: String,
    /// Where it runs.
    pub body: String,
    /// Which Tilo this is.
    pub version: String,
    /// Tilo's home on the web ([`TILO_HOME`]): in a browser, where to get Tilo; on a computer that
    /// has it, just its address.
    pub home: String,
}

/// The words for Tilo running as `variant` . A browser tab is not "this computer": there it is
/// About Tilo.
pub fn about_copy(variant: AboutVariant) -> AboutCopy {
    let (title, body, home) = match variant {
        AboutVariant::Native => (
            "tilo-about-title",
            "tilo-about-body-native",
            "tilo-about-link-home",
        ),
        AboutVariant::Browser => (
            "tilo-about-title-browser",
            "tilo-about-body-browser",
            "tilo-about-link-get",
        ),
    };
    AboutCopy {
        title: t(title),
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

/// The About sheet's body: the mark, the words for `variant` and the links offered.
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
    // Tilo's home leads, offered everywhere: the deployment's own links follow.
    let mut row = Row::new()
        .with_gap(ui::tokens::Spacing::default().s4)
        .push(Box::new(Link::new(TILO_HOME).with_label(&copy.home)));
    for (label, url) in links.shown() {
        row = row.push(Box::new(Link::new(&url).with_label(&label)));
    }
    column.push(Box::new(row))
}

/// The About sheet: what this Tilo is, over the window it was asked from.
pub struct AboutSheet {
    sheet: ModalDialog<Column>,
    /// Set by the sheet's close control, Escape or a click outside it.
    dismissed: Rc<Cell<bool>>,
    rect: Rect,
}

impl AboutSheet {
    pub fn new(variant: AboutVariant, links: &AboutLinks) -> Self {
        let copy = about_copy(variant);
        let dismissed = Rc::new(Cell::new(false));
        let dismiss = Rc::clone(&dismissed);
        Self {
            sheet: ModalDialog::new(copy.title, about_view(variant, links))
                .with_test_id("tilo-about")
                .with_max_width(440.0)
                .on_dismiss(move || dismiss.set(true)),
            dismissed,
            rect: Rect::ZERO,
        }
    }

    /// Show the sheet, or take it away: the chrome's own `about` verb.
    pub fn set_about_open(&mut self, open: bool) {
        self.sheet.set_open(open);
    }

    pub fn is_about_open(&self) -> bool {
        self.sheet.is_open()
    }

    /// Apply what the person dismissed since the last call; true when the sheet closed.
    pub fn settle(&mut self) -> bool {
        if self.dismissed.replace(false) {
            self.sheet.set_open(false);
            return true;
        }
        false
    }
}

impl Widget for AboutSheet {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The sheet is absolute and covers the host on its own, out of the column's flow.
        self.sheet.build_layout(taffy, theme, map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.sheet.apply_layout(taffy, node, origin);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.sheet.handle_event(event, ctx);
        self.settle();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.sheet]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.sheet]
    }

    fn traps_focus(&self) -> bool {
        self.is_about_open()
    }

    fn name(&self) -> &'static str {
        "AboutSheet"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_about_sheet_says_where_this_tilo_runs;

        phase empty_or_non_web_links_are_not_offered;
        {
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

        phase the_browser_says_it_runs_in_the_browser;
        {
            ui::i18n::I18nManager::init("en-US");
            let browser = about_copy(AboutVariant::Browser);
            let native = about_copy(AboutVariant::Native);
            assert_eq!(browser.lead, native.lead);
            assert!(browser.lead.contains("Tilo"), "{}", browser.lead);
            assert!(browser.body.contains("browser"), "{}", browser.body);
            assert!(!native.body.contains("browser"), "{}", native.body);
            // A browser tab is not "this computer".
            for words in [&browser.title, &browser.body] {
                assert!(
                    !words.contains("computer") || words.contains("own computer"),
                    "{words}"
                );
            }
            assert!(browser.title.contains("Tilo"), "{}", browser.title);
            assert!(native.title.contains("computer"), "{}", native.title);
            // Both say which Tilo this is, and where to find it on the web; the browser says that is
            // where to get it.
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

        phase the_sheet_opens_on_the_chromes_verb_and_closes_on_dismiss;
        {
            ui::i18n::I18nManager::init("en-US");
            let mut sheet = AboutSheet::new(AboutVariant::Browser, &AboutLinks::default());
            assert!(!sheet.is_about_open());
            sheet.set_about_open(true);
            assert!(sheet.is_about_open());
            sheet.set_about_open(false);
            assert!(!sheet.is_about_open());

            sheet.set_about_open(true);
            sheet.dismissed.set(true);
            assert!(sheet.settle(), "the dismissal closed the sheet");
            assert!(!sheet.is_about_open());
            assert!(!sheet.settle(), "a settled sheet asks for nothing more");
        }
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
}
