//! The one menu behind the top bar's workspace name: everything about where
//! the person is, in one place. Which workspace (the others, to switch to),
//! where it is hosted, joining or leaving one, renaming it, the overview and
//! a new desk.
//!
//! [`WorkspaceMenuModel`] is what the host knows when the menu opens; the
//! menu is a [`ContextMenu`] of it, and what the person chose comes back as a
//! [`WorkspaceMenuChoice`] for the host to carry out. Joining and leaving are
//! the host's own flows: it lists them as [`WorkspaceMenuModel::actions`]
//! and receives [`WorkspaceMenuChoice::Action`] by id.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{ContextMenu, MenuItem, TaffyTreeExt as _, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::i18n::{t, t_args};

/// One workspace the menu offers, by the host's own key for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceChoice {
    pub key: String,
    pub name: String,
    /// The workspace shown now: listed with a check, choosing it does
    /// nothing.
    pub current: bool,
}

/// A command the host offers about workspaces (join one, leave this one, a
/// new one), carried out by the host when chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceAction {
    pub id: String,
    pub title: String,
    /// Losing something: leaving the workspace.
    pub destructive: bool,
}

/// What the menu shows, as the host knows it when it opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceMenuModel {
    pub workspaces: Vec<WorkspaceChoice>,
    /// The site the workspace is hosted by, as the person reached it
    /// ("tilo.coffee"); `None` on the person's own computer.
    pub host: Option<String>,
    /// Join a workspace, leave this one, start a new one.
    pub actions: Vec<WorkspaceAction>,
    /// The person may rename the workspace.
    pub can_rename: bool,
    /// The person may add a desk to it.
    pub can_add_desk: bool,
}

/// What the person chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceMenuChoice {
    /// Go to the workspace with this key.
    Switch(String),
    Rename,
    Overview,
    NewDesk,
    /// The host's own command, by id.
    Action(String),
}

/// The menu behind the bar's workspace name.
#[derive(Default)]
pub struct WorkspaceMenu {
    menu: ContextMenu,
    /// What each row of `menu` stands for, `None` for separators and
    /// captions.
    rows: Vec<Option<WorkspaceMenuChoice>>,
    chosen: Option<WorkspaceMenuChoice>,
    rect: Rect,
}

impl WorkspaceMenu {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rows `model` makes, each with what choosing it means.
    fn rows(model: &WorkspaceMenuModel) -> Vec<(MenuItem, Option<WorkspaceMenuChoice>)> {
        let mut rows: Vec<(MenuItem, Option<WorkspaceMenuChoice>)> = Vec::new();
        for workspace in &model.workspaces {
            rows.push((
                MenuItem::selection(&workspace.name).with_checked(workspace.current),
                (!workspace.current).then(|| WorkspaceMenuChoice::Switch(workspace.key.clone())),
            ));
        }
        if let Some(host) = &model.host {
            // A caption under the workspaces, not a command.
            rows.push((
                MenuItem::caption(t_args(
                    "workspace-menu-hosted-by",
                    &[("host", host.as_str().into())],
                )),
                None,
            ));
        }
        if !model.actions.is_empty() {
            rows.push((MenuItem::separator(), None));
            for action in &model.actions {
                let item = if action.destructive {
                    MenuItem::destructive_selection(&action.title)
                } else {
                    MenuItem::selection(&action.title)
                };
                rows.push((item, Some(WorkspaceMenuChoice::Action(action.id.clone()))));
            }
        }
        rows.push((MenuItem::separator(), None));
        if model.can_rename {
            rows.push((
                MenuItem::selection(t("workspace-menu-rename")).with_icon(Icon::Pencil),
                Some(WorkspaceMenuChoice::Rename),
            ));
        }
        rows.push((
            MenuItem::selection(t("workspace-menu-overview"))
                .with_icon(crate::topbar::TopBarChrome::overview_glyph_icon()),
            Some(WorkspaceMenuChoice::Overview),
        ));
        if model.can_add_desk {
            rows.push((
                MenuItem::selection(t("workspace-menu-new-desk")).with_icon(Icon::Plus),
                Some(WorkspaceMenuChoice::NewDesk),
            ));
        }
        rows
    }

    /// Open the menu for `model` under `anchor` (the workspace name) within
    /// `viewport`, or close it when it is open.
    pub fn toggle(&mut self, model: &WorkspaceMenuModel, anchor: Rect, viewport: Rect) {
        if self.menu.is_open() {
            self.close();
            return;
        }
        let (items, rows): (Vec<_>, Vec<_>) = Self::rows(model).into_iter().unzip();
        self.menu.set_items(items);
        self.rows = rows;
        self.menu.open_anchored(anchor, viewport);
        self.menu.focus_first_action();
    }

    pub fn close(&mut self) {
        self.menu.close();
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// The labels of the rows shown, in order (separators as `None`).
    pub fn labels(&self) -> Vec<Option<String>> {
        self.menu
            .items()
            .iter()
            .map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Choose the row labelled `label`, as a click on it would.
    pub fn choose(&mut self, label: &str) -> bool {
        let Some(index) = self
            .menu
            .items()
            .iter()
            .position(|item| item.label() == Some(label))
        else {
            return false;
        };
        self.menu.click_item(index);
        self.settle();
        true
    }

    /// What the person chose since the last call; the menu has closed.
    pub fn take_choice(&mut self) -> Option<WorkspaceMenuChoice> {
        self.settle();
        self.chosen.take()
    }

    fn settle(&mut self) {
        if let Some(index) = self.menu.take_selection() {
            self.menu.close();
            if let Some(Some(choice)) = self.rows.get(index) {
                self.chosen = Some(choice.clone());
            }
        }
    }
}

impl Widget for WorkspaceMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let menu = self.menu.build_layout(taffy, theme, map);
        // Over the whole host, out of its flow, like the Tilo menu.
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
        taffy.widget_node(style, &[menu], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [menu] = taffy.children(node).unwrap_or_default()[..] {
            self.menu.apply_layout(taffy, menu, inner);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [menu] = nodes[..] else {
            return;
        };
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        }
        self.settle();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.menu]
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    fn name(&self) -> &'static str {
        "WorkspaceMenu"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> WorkspaceMenuModel {
        WorkspaceMenuModel {
            workspaces: vec![
                WorkspaceChoice {
                    key: "1".into(),
                    name: "Lounge".into(),
                    current: true,
                },
                WorkspaceChoice {
                    key: "2".into(),
                    name: "Home".into(),
                    current: false,
                },
            ],
            host: Some("tilo.coffee".into()),
            actions: vec![
                WorkspaceAction {
                    id: "join-workspace".into(),
                    title: "Join a workspace…".into(),
                    destructive: false,
                },
                WorkspaceAction {
                    id: "leave-workspace".into(),
                    title: "Leave".into(),
                    destructive: true,
                },
            ],
            can_rename: false,
            can_add_desk: true,
        }
    }

    fn opened(model: &WorkspaceMenuModel) -> WorkspaceMenu {
        ui::i18n::I18nManager::init("en-US");
        let mut menu = WorkspaceMenu::new();
        menu.toggle(
            model,
            Rect::new(40.0, 2.0, 140.0, 22.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        menu
    }

    #[test]
    fn the_menu_lists_workspaces_host_actions_overview_and_a_new_desk() {
        let menu = opened(&model());
        assert!(menu.is_open());
        let labels: Vec<String> = menu.labels().into_iter().flatten().collect();
        assert_eq!(
            labels,
            [
                "Lounge",
                "Home",
                "Hosted by tilo.coffee",
                "Join a workspace…",
                "Leave",
                "Overview",
                "New desk",
            ]
        );
        assert!(
            !labels.iter().any(|label| label == "Rename workspace"),
            "no Rename for someone who may not"
        );
    }

    #[test]
    fn rename_is_offered_to_whoever_may_rename() {
        let menu = opened(&WorkspaceMenuModel {
            can_rename: true,
            ..model()
        });
        assert!(menu.labels().contains(&Some("Rename workspace".to_owned())));
    }

    #[test]
    fn choosing_a_row_says_what_it_means_once_and_closes_the_menu() {
        for (label, choice) in [
            ("Home", Some(WorkspaceMenuChoice::Switch("2".into()))),
            ("Overview", Some(WorkspaceMenuChoice::Overview)),
            ("New desk", Some(WorkspaceMenuChoice::NewDesk)),
            (
                "Leave",
                Some(WorkspaceMenuChoice::Action("leave-workspace".into())),
            ),
            ("Lounge", None),
        ] {
            let mut menu = opened(&model());
            assert!(menu.choose(label), "{label} is a row");
            assert_eq!(menu.take_choice(), choice, "{label}");
            assert_eq!(menu.take_choice(), None, "{label} is said once");
        }
    }

    #[test]
    fn the_host_caption_is_not_a_command() {
        let mut menu = opened(&model());
        menu.choose("Hosted by tilo.coffee");
        assert_eq!(menu.take_choice(), None);
    }

    #[test]
    fn toggling_again_closes() {
        let mut menu = opened(&model());
        menu.toggle(&model(), Rect::ZERO, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(!menu.is_open());
    }
}
