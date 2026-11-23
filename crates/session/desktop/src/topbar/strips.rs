//! Where you are, at the bar's left end: the workspace, then the strip
//! shown ("Home › Research ⌄"). The strip's name is one target, a menu as the
//! workspace's name is: it lists the workspace's strips under itself and makes
//! a new one. The bar never counts who is on them: who is where shows as faces
//! on the strip column's labels.
use super::{Affine, BAR_CONTROL_PX, Rect, Scene, TextRenderer, TextRole, Theme, TopBarChrome};
/// The strip's separator: the chevron that opens the strip switcher.
static STRIP_GLYPH: &[ui::icons::Icon] = &[ui::icons::Icon::ChevronRight];

use tilo_desktop_model::{Layout, StripId};
use ui::text_render::TextStyle;

#[derive(Default)]
pub(super) struct BarStrips {
    /// The active workspace's strips, in order.
    strips: Vec<(StripId, String)>,
    active: Option<StripId>,
    /// What the bar calls the strip shown: its own name, or the place it stands in.
    shown: Option<String>,
}

/// The drawn chevron between the workspace and the strip's name.
const STRIP_CHEVRON_PX: f64 = 12.0;

/// Everything about the strips that changes what the bar paints.
#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct StripsKey {
    shown: Option<String>,
}

impl TopBarChrome {
    pub(super) fn sync_strips(&mut self, tree: &Layout) {
        let workspace = tree.workspaces.iter().find(|w| w.id() == tree.active);
        self.strips.strips = workspace
            .map(|w| {
                w.strips
                    .iter()
                    .map(|strip| (strip.id, strip.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        // A bar on a display names the strip that display shows; the full bar stands on the display
        // the person works on, which shows the workspace's active strip.
        self.strips.active = self
            .display
            .and_then(|display| tree.strip_shown_on(display))
            .or_else(|| tree.active_strip_id())
            .filter(|id| self.strips.strips.iter().any(|(strip, _)| strip == id))
            .or_else(|| self.strips.strips.first().map(|(id, _)| *id));
        self.strips.shown = self.strips.active.and_then(|active| {
            let (at, (_, name)) = self
                .strips
                .strips
                .iter()
                .enumerate()
                .find(|(_, (id, _))| *id == active)?;
            Some(tilo_session_core::desktop::names::strip_named(name, at))
        });
    }

    /// The shown strip's name, which the bar names after the workspace (Home, a chevron, Research).
    /// A strip nobody named is called by the place it stands in ("Strip 1"), so even a workspace of
    /// one strip says it has strips, and the name is where they are listed and added.
    pub(super) fn strip_name(&self) -> Option<&str> {
        self.strips.shown.as_deref()
    }

    /// The gap either side of the chevron before the strip's name.
    fn strip_name_gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s1)
    }

    /// The width the strip's name takes after the workspace's pill at its natural length: the
    /// separator, its gaps and the name; zero when the bar names no strip. The workspace name
    /// leaves only its own width.
    pub(super) fn strips_natural_width(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let Some(name) = self.strip_name() else {
            return 0.0;
        };
        let style = Self::strip_text_style(theme);
        let name = text_renderer.measure_style(name, None, &style).0;
        STRIP_CHEVRON_PX
            + f64::from(name)
            + 2.0 * Self::strip_name_gap(theme)
            + Self::strip_menu_glyph_width(theme)
    }

    /// The glyph closing the strip's name, as the one closing the workspace's: it opens a menu.
    fn strip_menu_glyph_width(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.icon_control) + f64::from(theme.tokens.spacing.rhythm.tight)
    }

    /// The strip's chip showing `name` with `label_width` of room for it.
    pub(super) fn strip_chip<'a>(
        &self,
        name: &'a str,
        label_width: f64,
    ) -> ui::widget::BarChipFace<'a> {
        ui::widget::BarChipFace::new(name, &[])
            .icons(STRIP_GLYPH)
            .label(name)
            .trailing(Self::WORKSPACE_MENU_ICON)
            .label_width(label_width)
            .at_height(BAR_CONTROL_PX)
            .hovered(self.hovered_control().as_deref() == Some(Self::CONTROL_STRIP_NAME))
            .focused(
                self.focus_ring_visible()
                    && self.focused_control.as_deref() == Some(Self::CONTROL_STRIP_NAME),
            )
    }

    /// Where the shown strip's name stands between `x0` and `x1`, after the workspace pill: the
    /// separator and the name elided to the room left, one target, which opens the strip switcher
    /// ([`ChromeIntent::ShowStrips`]). Nowhere when the room cannot hold it.
    pub(super) fn place_strip_name(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        span: (f64, f64),
    ) {
        self.ws_strip_rect = Rect::ZERO;
        self.placed.strip = None;
        let Some(name) = self.strip_name().map(str::to_owned) else {
            return;
        };
        let (x0, x1) = span;
        let gap = Self::strip_name_gap(theme);
        let label_width =
            (x1 - x0 - gap - STRIP_CHEVRON_PX - gap - Self::strip_menu_glyph_width(theme)).max(0.0);
        let width = self
            .strip_chip(&name, label_width)
            .width(text_renderer, theme);
        if x1 - x0 < width {
            return;
        }
        self.ws_strip_rect = ui::widget::BarChipFace::place_at(x0, center_y, width, BAR_CONTROL_PX);
        self.placed.strip = Some((name, label_width));
    }

    /// Paint the strip's name where [`Self::place_strip_name`] stood it: the separator dim, the
    /// name after it.
    pub(super) fn paint_strip_name(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let Some((name, label_width)) = self.placed.strip.as_ref() else {
            return;
        };
        let _ = self.strip_chip(name, *label_width).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            self.ws_strip_rect,
            super::bar_ground(theme),
        );
    }

    /// Where the strip's name was painted this frame, at the bar's height: its part's bounds.
    pub(super) fn strips_painted(&self) -> Vec<Rect> {
        std::iter::once(self.ws_strip_rect)
            .filter(|rect| !rect.is_zero_area())
            .map(|rect| {
                Rect::new(
                    rect.x0 - 4.0,
                    self.rect.y0 - 2.0,
                    rect.x1 + 4.0,
                    self.rect.y1 + 2.0,
                )
            })
            .collect()
    }

    pub(super) fn strips_key(&self) -> StripsKey {
        StripsKey {
            shown: self.strip_name().map(str::to_owned),
        }
    }

    fn strip_text_style(theme: &Theme) -> TextStyle {
        theme.tokens.typography.style(TextRole::Label)
    }
}

#[cfg(test)]
mod tests {
    use super::super::ChromeIntent;
    use super::*;
    use crate::chrome::{DesktopChromeSnapshot, DesktopChromeSource};
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_desktop_model::{Member, Strip, Tab, TileContent, WindowGroup, WindowWidth};
    use tilo_desktop_model::{Workspace, WorkspaceId};
    use ui::headless::Framed;
    use ui::widget::Widget;

    struct Host {
        tree: RefCell<Layout>,
        intents: RefCell<Vec<ChromeIntent>>,
    }

    impl DesktopChromeSource for Host {
        fn snapshot(&self) -> DesktopChromeSnapshot {
            DesktopChromeSnapshot::default()
        }
        fn tree(&self) -> Layout {
            self.tree.borrow().clone()
        }
        fn intent(&self, intent: ChromeIntent) -> bool {
            self.intents.borrow_mut().push(intent);
            true
        }
    }

    fn group(id: u64) -> WindowGroup {
        WindowGroup {
            id,
            active_tab: id.saturating_add(1000),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
            tabs: vec![Tab {
                id: id.saturating_add(1000),
                title: "Notes".into(),
                app_title: None,
                tile: TileContent::AppPlaceholder {
                    kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                    label: "Notes".into(),
                },
                view: String::new(),
                pinned: false,
            }],
        }
    }

    /// A workspace of three named strips, the first shown, with people on the other two.
    fn tree() -> Layout {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Home");
        let mut games = Strip::new(StripId(11), "Games");
        games.window_groups = vec![group(1)];
        let mut quiet = Strip::new(StripId(12), "Quiet");
        quiet.window_groups = vec![group(2)];
        workspace.strips = vec![Strip::new(StripId(10), "Main"), games, quiet];
        let ada = Member::named(collab_types::MemberId::person("ada"), "Ada");
        let bo = Member::named(collab_types::MemberId::person("bo"), "Bo");
        workspace.members_here([ada, bo]);
        workspace.present[0].tab = Some(1001);
        workspace.present[1].tab = Some(1002);
        let mut tree = Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        };
        tree.active_strip
            .insert(WorkspaceId::from_u128(1), StripId(10));
        tree
    }

    fn bar(tree: &Layout, width: u32) -> (Framed<TopBarChrome>, Rc<Host>) {
        let chrome = Rc::new(Host {
            tree: RefCell::new(tree.clone()),
            intents: RefCell::default(),
        });
        let bar =
            TopBarChrome::from_tree_with_local_user(tree, "me".into()).with_chrome(chrome.clone());
        let frame = Framed::new(ui::State::new(), bar, (width, 480), Theme::dark());
        (frame, chrome)
    }

    #[test]
    fn one_strip_picker_the_bar_names_only_the_strip_shown_and_opens_the_switcher() {
        let tree = tree();
        let (mut frame, chrome) = bar(&tree, 1600);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        // Only the strip shown, named after the workspace.
        assert_eq!(frame.strip_name(), Some("Main"));
        let crumb = frame.ws_strip_rect;
        assert!(!crumb.is_zero_area(), "Workspace › Strip");
        assert!(crumb.x0 >= frame.ws_pill_rect.x1 - 0.5);
        // No other strip, and no count of who is on one, anywhere in the bar: not as a target, not
        // for assistive technology.
        let rects = frame.test_rects();
        for other in ["Games", "Quiet"] {
            assert!(
                rects
                    .iter()
                    .all(|(name, rect)| !name.contains(other) || rect.is_zero_area()),
                "the bar lists no other strip: {rects:?}"
            );
        }
        let spoken = format!("{:?}", frame.accessibility());
        assert!(
            !spoken.contains("Games") && !spoken.contains("Quiet"),
            "nothing about the other strips is spoken from the bar"
        );
        assert!(
            !spoken.contains("people there") && !spoken.contains("person there"),
            "no people counts in the bar"
        );
        // The crumb opens the switcher, the one place strips are picked.
        Framed::harness_mut(&mut frame).click_at(crumb.center().x, crumb.center().y);
        Framed::harness_mut(&mut frame).poll_frame().unwrap();
        assert_eq!(
            chrome.intents.borrow().as_slice(),
            [ChromeIntent::ShowStrips]
        );
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }

    /// A bar names the strip its own display shows: on two displays, the one the person works on
    /// shows the active strip, the other the strip it was left showing.
    #[test]
    fn a_bar_names_the_strip_its_own_display_shows() {
        let mut tree = tree();
        let displays = vec![
            tilo_desktop_model::partition::Display {
                id: 0,
                monitor: "panel".into(),
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                primary: false,
            },
            tilo_desktop_model::partition::Display {
                id: 1,
                monitor: "monitor".into(),
                x: 1280.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                primary: false,
            },
        ];
        tree.outputs.reconcile(&tree.workspaces, &displays);
        tree.outputs.set_focused(1);
        assert_eq!(
            tree.outputs.focused_display(),
            Some(1),
            "the monitor is in use"
        );
        assert!(
            tree.outputs
                .set_shown(0, WorkspaceId::from_u128(1), StripId(11))
        );
        tree.active_strip
            .insert(WorkspaceId::from_u128(1), StripId(10));

        let full = TopBarChrome::from_tree_with_local_user(&tree, "me".into());
        assert_eq!(full.strip_name(), Some("Main"), "the display in use");

        let mut other = TopBarChrome::from_tree_with_local_user(&tree, "me".into())
            .compact()
            .on_display(0);
        other.sync_from_tree(&tree);
        assert_eq!(other.strip_name(), Some("Games"), "its own display's strip");
    }

    #[test]
    fn a_narrow_bar_keeps_the_strip_named_and_passes_the_native_audit() {
        let (frame, _) = bar(&tree(), 360);
        let audit = Framed::harness(&frame).native_app_audit();
        assert!(audit.is_certified(), "{audit:?}");
    }
}
