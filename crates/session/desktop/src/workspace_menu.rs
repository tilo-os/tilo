//! The one menu behind the top bar's workspace name, and the menu a secondary press on the desk
//! opens where the pointer is: everything about where the person is, in one place. Which workspace
//! (the others, to switch to), where it is hosted, joining or leaving one, renaming it, the
//! overview and a new strip — and, opened on the desk, what the desk itself offers
//! ([`WorkspaceMenuModel::desk`]).
//! [`WorkspaceMenuModel`] is what the host knows when the menu opens; the menu is a [`ContextMenu`]
//! of it, and what the person chose comes back as a [`WorkspaceMenuChoice`] for the host to carry
//! out. Joining and leaving are the host's own flows: it lists them as
//! [`WorkspaceMenuModel::actions`] and receives [`WorkspaceMenuChoice::Action`] by id.

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
    /// The workspace shown now: listed with a check, choosing it does nothing.
    pub current: bool,
}

/// A command the host offers about workspaces (join one, leave this one, a new one), carried out by
/// the host when chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceAction {
    pub id: String,
    /// The action's name, not the sentence describing it.
    pub title: String,
    /// The action's own picture, as it declares one.
    pub icon: Option<Icon>,
    /// Losing something: leaving the workspace.
    pub destructive: bool,
}

impl WorkspaceAction {
    /// The row this command is, with its own picture.
    fn item(&self) -> MenuItem {
        let mut item = if self.destructive {
            MenuItem::destructive_selection(&self.title)
        } else {
            MenuItem::selection(&self.title)
        };
        if let Some(icon) = self.icon {
            item = item.with_icon(icon);
        }
        item
    }
}

/// Where a press opened the menu: the bar's workspace name, or the desk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuPlace {
    /// Behind the bar's workspace name: everything about where the person is, in one list.
    #[default]
    Bar,
    /// A secondary press on the desk: what the place itself offers, and the workspace's own
    /// commands in a submenu of their own.
    Desk,
}

/// How many of the desk's own verbs a menu offers before it stops being about the place the person
/// pressed.
const DESK_ROWS: usize = 6;

/// What the menu shows, as the host knows it when it opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceMenuModel {
    /// Where the press that opened it landed: the bar, or the desk.
    pub place: MenuPlace,
    /// What the place a press opened the menu on offers: the desk's own verbs, at most
    /// [`DESK_ROWS`] of them. Empty for the bar's menu, which stands on the workspace name rather
    /// than on the desk.
    pub desk: Vec<WorkspaceAction>,
    pub workspaces: Vec<WorkspaceChoice>,
    /// The site the workspace is hosted by, as the person reached it ("tilo.sh"); `None` on the
    /// person's own computer.
    pub host: Option<String>,
    /// Join a workspace, leave this one, start a new one.
    pub actions: Vec<WorkspaceAction>,
    pub can_rename: bool,
    pub can_add_strip: bool,
    /// Deleting the workspace shown: the person owns it and this computer is the only one holding
    /// it, so nothing of it is left anywhere.
    pub can_delete: bool,
}

/// What the person chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceMenuChoice {
    /// Go to the workspace with this key.
    Switch(String),
    Rename,
    Overview,
    NewStrip,
    /// The host's own command, by id.
    Action(String),
}

/// A row `model` makes: what it shows, what choosing it means, and — for a row that opens a submenu
/// — what each of the submenu's rows means.
struct Row {
    item: MenuItem,
    choice: Option<WorkspaceMenuChoice>,
    children: Vec<Option<WorkspaceMenuChoice>>,
}

impl Row {
    fn new(item: MenuItem, choice: Option<WorkspaceMenuChoice>) -> Self {
        Self {
            item,
            choice,
            children: Vec::new(),
        }
    }

    fn rule() -> Self {
        Self::new(MenuItem::separator(), None)
    }

    /// The row that opens `children` beside it.
    fn submenu(label: impl Into<String>, children: Vec<Self>) -> Self {
        let mut choices = Vec::with_capacity(children.len());
        let items = children
            .into_iter()
            .map(|child| {
                choices.push(child.choice);
                child.item
            })
            .collect();
        Self {
            item: MenuItem::submenu(label, items),
            choice: None,
            children: choices,
        }
    }
}

/// The menu behind the bar's workspace name, and the one a press on the desk opens: one list of the
/// place's commands, or of everything where the person is.
#[derive(Default)]
pub struct WorkspaceMenu {
    menu: ContextMenu,
    /// What each row of `menu` stands for, `None` for separators and captions.
    rows: Vec<Option<WorkspaceMenuChoice>>,
    /// What the rows of each submenu stand for, aligned with `rows` : empty where the row opens no
    /// submenu.
    nested: Vec<Vec<Option<WorkspaceMenuChoice>>>,
    chosen: Option<WorkspaceMenuChoice>,
    rect: Rect,
}

impl WorkspaceMenu {
    pub fn new() -> Self {
        Self::default()
    }

    /// The rows `model` makes, each with what choosing it means.
    fn rows(model: &WorkspaceMenuModel) -> Vec<Row> {
        match model.place {
            MenuPlace::Bar => Self::bar_rows(model),
            MenuPlace::Desk => Self::desk_rows(model),
        }
    }

    /// What the bar's menu offers: the workspaces, where the person is hosted, the workspace's
    /// commands, and what changes them.
    fn bar_rows(model: &WorkspaceMenuModel) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        for workspace in &model.workspaces {
            rows.push(Row::new(
                MenuItem::selection(&workspace.name).with_checked(workspace.current),
                (!workspace.current).then(|| WorkspaceMenuChoice::Switch(workspace.key.clone())),
            ));
        }
        if let Some(host) = &model.host {
            rows.push(Self::hosted_by(host));
        }
        if !model.actions.is_empty() {
            rows.push(Row::rule());
            for action in &model.actions {
                rows.push(Row::new(
                    action.item(),
                    Some(WorkspaceMenuChoice::Action(action.id.clone())),
                ));
            }
        }
        rows.push(Row::rule());
        if model.can_rename {
            rows.push(Self::rename());
        }
        rows.push(Self::overview());
        if model.can_add_strip {
            rows.push(Self::new_strip());
        }
        if model.can_delete {
            rows.push(Row::rule());
            rows.push(Self::delete());
        }
        rows
    }

    /// What a press on the desk offers: the place's own commands, then what makes the place, and
    /// the workspace's own commands in a submenu beside it. Nothing here names the workspace the
    /// person is already in: a menu on the desk says what the desk does, not where they are.
    fn desk_rows(model: &WorkspaceMenuModel) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        for action in model.desk.iter().take(DESK_ROWS) {
            rows.push(Row::new(
                action.item(),
                Some(WorkspaceMenuChoice::Action(action.id.clone())),
            ));
        }
        if !rows.is_empty() {
            rows.push(Row::rule());
        }
        if model.can_add_strip {
            rows.push(Self::new_strip());
        }
        rows.push(Self::overview());
        let mut workspace: Vec<Row> = Vec::new();
        for other in model
            .workspaces
            .iter()
            .filter(|workspace| !workspace.current)
        {
            workspace.push(Row::new(
                MenuItem::selection(&other.name),
                Some(WorkspaceMenuChoice::Switch(other.key.clone())),
            ));
        }
        let host = model.host.as_ref().map(|host| Self::hosted_by(host));
        workspace.extend(host);
        // Losing the workspace goes last, apart from what joins or renames it: a row a slip of the
        // pointer must not land on.
        let (leaving, staying): (Vec<&WorkspaceAction>, Vec<&WorkspaceAction>) =
            model.actions.iter().partition(|action| action.destructive);
        if !staying.is_empty() {
            if !workspace.is_empty() {
                workspace.push(Row::rule());
            }
            for action in staying {
                workspace.push(Row::new(
                    action.item(),
                    Some(WorkspaceMenuChoice::Action(action.id.clone())),
                ));
            }
        }
        if model.can_rename {
            if !workspace.is_empty() {
                workspace.push(Row::rule());
            }
            workspace.push(Self::rename());
        }
        if !leaving.is_empty() {
            if !workspace.is_empty() {
                workspace.push(Row::rule());
            }
            for action in leaving {
                workspace.push(Row::new(
                    action.item(),
                    Some(WorkspaceMenuChoice::Action(action.id.clone())),
                ));
            }
        }
        if !workspace.is_empty() {
            rows.push(Row::submenu(t("workspace-menu-workspace"), workspace));
        }
        if model.can_delete {
            rows.push(Row::rule());
            rows.push(Self::delete());
        }
        rows
    }

    /// The caption naming the site the workspace is hosted by, not a command.
    fn hosted_by(host: &str) -> Row {
        Row::new(
            MenuItem::caption(t_args("workspace-menu-hosted-by", &[("host", host.into())])),
            None,
        )
    }

    fn rename() -> Row {
        Row::new(
            MenuItem::selection(t("workspace-menu-rename")).with_icon(Icon::Pencil),
            Some(WorkspaceMenuChoice::Rename),
        )
    }

    fn overview() -> Row {
        Row::new(
            MenuItem::selection(t("workspace-menu-overview"))
                .with_icon(crate::topbar::TopBarChrome::overview_glyph_icon()),
            Some(WorkspaceMenuChoice::Overview),
        )
    }

    fn new_strip() -> Row {
        Row::new(
            MenuItem::selection(t("workspace-menu-new-strip")).with_icon(Icon::Plus),
            Some(WorkspaceMenuChoice::NewStrip),
        )
    }

    /// Deleting the workspace stands last and apart: nothing else is reached for on the way to it.
    fn delete() -> Row {
        Row::new(
            MenuItem::destructive_selection(t("workspace-menu-delete")).with_icon(Icon::Trash2),
            Some(WorkspaceMenuChoice::Action(
                crate::desktop_host::DELETE_WORKSPACE.to_owned(),
            )),
        )
    }

    /// Open the menu for `model` under `anchor` (the workspace name) within `viewport` , or close
    /// it when it is open.
    pub fn toggle(&mut self, model: &WorkspaceMenuModel, anchor: Rect, viewport: Rect) {
        if self.menu.is_open() {
            self.close();
            return;
        }
        self.show(model, anchor, viewport);
    }

    /// Open the menu for `model` with its first row at `at` (a secondary press on the desk) within
    /// `viewport` , or close it when it is open: the same menu, standing where the person pressed.
    pub fn open_at(&mut self, model: &WorkspaceMenuModel, at: Point, viewport: Rect) {
        if self.menu.is_open() {
            self.close();
            return;
        }
        self.show(model, Rect::new(at.x, at.y, at.x, at.y), viewport);
    }

    /// Draw the rows `model` makes, anchored `anchor` , kept inside `viewport` (the [`ContextMenu`]
    /// flips it above or left at an edge).
    fn show(&mut self, model: &WorkspaceMenuModel, anchor: Rect, viewport: Rect) {
        let mut items = Vec::new();
        let mut rows = Vec::new();
        let mut nested = Vec::new();
        for row in Self::rows(model) {
            items.push(row.item);
            rows.push(row.choice);
            nested.push(row.children);
        }
        self.menu.set_items(items);
        self.rows = rows;
        self.nested = nested;
        self.menu.open_anchored(anchor, viewport);
        self.menu.focus_first_action();
    }

    pub fn close(&mut self) {
        self.menu.close();
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Where the panel stands while it is open: as big as its rows, which no anchor says, so a
    /// press that misses it can close it.
    pub fn panel_rect(&self) -> Option<Rect> {
        self.menu.is_open().then(|| self.menu.get_rect())
    }

    /// The labels of the rows shown, in order (separators as `None` ).
    pub fn labels(&self) -> Vec<Option<String>> {
        self.menu
            .items()
            .iter()
            .map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Choose the row labelled `label` , as a click on it would.
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

    /// Choose the row `child` of the submenu `label` opens, as a click on it does.
    pub fn choose_in(&mut self, label: &str, child: &str) -> bool {
        let Some(row) = self
            .menu
            .items()
            .iter()
            .position(|item| item.label() == Some(label))
        else {
            return false;
        };
        let Some(nested) = self
            .menu
            .items()
            .get(row)
            .and_then(MenuItem::submenu_items)
            .and_then(|items| items.iter().position(|item| item.label() == Some(child)))
        else {
            return false;
        };
        self.menu.choose_submenu_item(row, nested);
        self.settle();
        true
    }

    /// What the person chose since the last call; the menu has closed.
    pub fn take_choice(&mut self) -> Option<WorkspaceMenuChoice> {
        self.settle();
        self.chosen.take()
    }

    fn settle(&mut self) {
        if let Some((row, nested)) = self.menu.take_submenu_selection() {
            self.menu.close();
            if let Some(Some(choice)) = self
                .nested
                .get(row)
                .and_then(|children| children.get(nested))
            {
                self.chosen = Some(choice.clone());
            }
            return;
        }
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
            place: MenuPlace::Bar,
            desk: Vec::new(),
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
            host: Some("tilo.sh".into()),
            actions: vec![
                WorkspaceAction {
                    id: "join-workspace".into(),
                    title: "Join a workspace".into(),
                    icon: Some(Icon::Link),
                    destructive: false,
                },
                WorkspaceAction {
                    id: "leave-workspace".into(),
                    title: "Leave workspace".into(),
                    icon: Some(Icon::ArrowUpRight),
                    destructive: true,
                },
            ],
            can_rename: false,
            can_add_strip: true,
            can_delete: false,
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
    fn the_menu_lists_workspaces_host_actions_overview_and_a_new_strip() {
        let menu = opened(&model());
        assert!(menu.is_open());
        let labels: Vec<String> = menu.labels().into_iter().flatten().collect();
        assert_eq!(
            labels,
            [
                "Lounge",
                "Home",
                "Hosted by tilo.sh",
                "Join a workspace",
                "Leave workspace",
                "Overview",
                "New strip",
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
            ("New strip", Some(WorkspaceMenuChoice::NewStrip)),
            (
                "Leave workspace",
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
        menu.choose("Hosted by tilo.sh");
        assert_eq!(menu.take_choice(), None);
    }

    #[test]
    fn deleting_stands_last_and_after_leaving() {
        let plain = opened(&model());
        assert!(
            !plain
                .labels()
                .contains(&Some("Delete workspace".to_owned())),
            "nothing offers to delete a workspace that stays elsewhere"
        );
        let mut menu = opened(&WorkspaceMenuModel {
            can_delete: true,
            ..model()
        });
        let labels: Vec<String> = menu.labels().into_iter().flatten().collect();
        assert_eq!(labels.last().map(String::as_str), Some("Delete workspace"));
        assert!(menu.choose("Delete workspace"));
        assert_eq!(
            menu.take_choice(),
            Some(WorkspaceMenuChoice::Action("delete-workspace".into()))
        );
    }

    #[test]
    fn toggling_again_closes() {
        let mut menu = opened(&model());
        menu.toggle(&model(), Rect::ZERO, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert!(!menu.is_open());
    }

    /// The menu a press on the desk opens: the place's own verbs, a new strip, the overview, and
    /// the workspace's commands beside them.
    fn desk_model() -> WorkspaceMenuModel {
        WorkspaceMenuModel {
            place: MenuPlace::Desk,
            desk: (0..8)
                .map(|index| WorkspaceAction {
                    id: format!("launchpad-{index}"),
                    title: format!("Verb {index}"),
                    icon: None,
                    destructive: false,
                })
                .collect(),
            ..model()
        }
    }

    fn desk_opened() -> WorkspaceMenu {
        ui::i18n::I18nManager::init("en-US");
        let mut menu = WorkspaceMenu::new();
        menu.open_at(
            &desk_model(),
            Point::new(640.0, 400.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        menu
    }

    #[test]
    fn a_press_on_the_desk_offers_the_place_and_keeps_the_workspace_in_a_submenu() {
        let menu = desk_opened();
        assert!(menu.is_open());
        let labels: Vec<String> = menu.labels().into_iter().flatten().collect();
        assert_eq!(
            labels,
            [
                "Verb 0",
                "Verb 1",
                "Verb 2",
                "Verb 3",
                "Verb 4",
                "Verb 5",
                "New strip",
                "Overview",
                "Workspace",
            ],
            "at most a menu's worth of the place's verbs, then what makes the place, \
             and the workspace's own commands beside it"
        );
        assert!(
            !labels.iter().any(|label| label == "Lounge"),
            "the workspace the person is in is not a row of its own"
        );
    }

    #[test]
    fn the_workspaces_submenu_keeps_leaving_last_and_apart() {
        let mut menu = opened(&WorkspaceMenuModel {
            can_rename: true,
            ..desk_model()
        });
        assert!(menu.choose_in("Workspace", "Home"));
        assert_eq!(
            menu.take_choice(),
            Some(WorkspaceMenuChoice::Switch("2".into()))
        );

        let menu = opened(&WorkspaceMenuModel {
            can_rename: true,
            ..desk_model()
        });
        let submenu = menu.menu.items().last().and_then(MenuItem::submenu_items);
        let Some(submenu) = submenu else {
            panic!("the desk menu offers the workspaces beside it");
        };
        let labels: Vec<Option<String>> = submenu
            .iter()
            .map(|item| item.label().map(str::to_owned))
            .collect();
        assert_eq!(
            labels
                .iter()
                .filter_map(|label| label.as_deref())
                .filter(|label| !label.is_empty())
                .collect::<Vec<_>>(),
            [
                "Home",
                "Hosted by tilo.sh",
                "Join a workspace",
                "Rename workspace",
                "Leave workspace",
            ]
        );
        assert_eq!(
            labels.last().cloned(),
            Some(Some("Leave workspace".to_owned())),
            "leaving goes last"
        );

        let mut menu = opened(&desk_model());
        assert!(menu.choose_in("Workspace", "Leave workspace"));
        assert_eq!(
            menu.take_choice(),
            Some(WorkspaceMenuChoice::Action("leave-workspace".into()))
        );
    }

    #[test]
    fn a_second_press_on_the_desk_closes_the_menu_it_opened() {
        let mut menu = desk_opened();
        assert!(menu.choose("Verb 0"));
        assert_eq!(
            menu.take_choice(),
            Some(WorkspaceMenuChoice::Action("launchpad-0".into()))
        );
        let desk = desk_model();
        menu.open_at(
            &desk,
            Point::new(10.0, 10.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        assert!(menu.is_open(), "opening it again after choosing opens it");
        menu.open_at(
            &desk,
            Point::new(10.0, 10.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        assert!(!menu.is_open(), "a second press on the desk closes it");
    }
}
