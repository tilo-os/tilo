//! The workspace switcher behind the top bar's workspace name, and the menu a secondary press on
//! the shell opens where the pointer is.
//!
//! The switcher is only that: every workspace the person has, the one they are in marked, the two
//! ways to another one (a new one, or joining one), and the way into the shown workspace's own
//! settings, which hold its name, its people and leaving it. The person's own desktop, where the
//! session offers one, is no workspace of theirs: it stands above them, apart, as what it is. The
//! Tilo mark opens the overview.
//!
//! Behind the strip's name beside it ([`MenuPlace::Strips`]) the same menu lists the workspace's
//! strips, the shown one marked, and makes a new one, next to where the person pressed.
//!
//! Opened on the shell ([`MenuPlace::Shell`]), the menu says what the shell itself offers: its own
//! verbs, and a new strip.
//!
//! [`WorkspaceMenuModel`] is what the host knows when the menu opens; the menu is a [`ContextMenu`]
//! of it, and what the person chose comes back as a [`WorkspaceMenuChoice`] for the host to carry
//! out.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use ui::icons::Icon;
use ui::theme::Theme;
use ui::widget::{ContextMenu, MenuItem, TaffyTreeExt as _, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use crate::i18n::t;

/// A command the host offers (making a workspace, joining one, a verb of the shell), carried out by
/// the host when chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceAction {
    pub id: String,
    /// The action's name, not the sentence describing it.
    pub title: String,
    /// The action's own picture, as it declares one.
    pub icon: Option<Icon>,
    /// Losing something.
    pub destructive: bool,
}

impl WorkspaceAction {
    /// The row this command is, with its own picture, choosing it.
    fn row(&self) -> Row {
        let mut item = if self.destructive {
            MenuItem::destructive_selection(&self.title)
        } else {
            MenuItem::selection(&self.title)
        };
        if let Some(icon) = self.icon {
            item = item.with_icon(icon);
        }
        Row::new(item, Some(WorkspaceMenuChoice::Action(self.id.clone())))
    }
}

/// One of the person's workspaces, as the switcher lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEntry {
    /// Its name, as the person knows it.
    pub name: String,
    /// The workspace the person is in: marked, and choosing it changes nothing.
    pub current: bool,
    /// Going there.
    pub choice: WorkspaceMenuChoice,
}

impl WorkspaceEntry {
    /// The row naming it, marked when it is the one the person is in.
    fn row(&self) -> Row {
        let item = MenuItem::selection(&self.name).with_checked(self.current);
        let choice = (!self.current).then(|| self.choice.clone());
        Row::new(item, choice)
    }
}

/// Where a press opened the menu: the bar's workspace name, or the shell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MenuPlace {
    /// Behind the bar's workspace name: the switcher.
    #[default]
    Bar,
    /// A secondary press on the shell: what the shell itself offers.
    Shell,
    /// Behind the bar's strip name: the workspace's strips, then a new one.
    Strips,
}

/// How many of the shell's own verbs a menu offers before it stops being about the place the person
/// pressed.
const SHELL_ROWS: usize = 6;

/// What the menu shows, as the host knows it when it opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceMenuModel {
    /// Where the press that opened it landed: the bar, or the shell.
    pub place: MenuPlace,
    /// What the shell offers where a press opened the menu on it: its own verbs, at most
    /// [`SHELL_ROWS`] of them. Unused by the switcher.
    pub shell: Vec<WorkspaceAction>,
    /// Every workspace the person has, the one they are in among them. Unused on the shell.
    pub workspaces: Vec<WorkspaceEntry>,
    /// The person's own desktop, where the session offers it: their private place, never one of the
    /// workspaces above, and shown apart from them. Unused on the shell and the strips menu.
    pub own_desktop: Option<WorkspaceAction>,
    /// The ways to another workspace: a new one, joining one. Unused on the shell.
    pub actions: Vec<WorkspaceAction>,
    /// The way into the shown workspace's own settings, last in the switcher.
    pub settings: Option<WorkspaceAction>,
    /// The shown workspace's strips, the one shown among them: the strips menu's rows.
    pub strips: Vec<WorkspaceEntry>,
    /// The person may add a strip to the workspace shown: the shell's and the strips menu offer
    /// one.
    pub can_add_strip: bool,
}

/// What the person chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceMenuChoice {
    /// Go to this workspace of the desktop, by its id.
    Switch(String),
    /// Show this strip of the workspace.
    Strip(tilo_desktop_model::StripId),
    /// Make a strip and show it.
    NewStrip,
    /// The host's own command, by id.
    Action(String),
}

/// A row `model` makes: what it shows, and what choosing it means.
struct Row {
    item: MenuItem,
    choice: Option<WorkspaceMenuChoice>,
}

impl Row {
    fn new(item: MenuItem, choice: Option<WorkspaceMenuChoice>) -> Self {
        Self { item, choice }
    }

    fn rule() -> Self {
        Self::new(MenuItem::separator(), None)
    }
}

/// The switcher behind the bar's workspace name, and the menu a press on the shell opens.
#[derive(Default)]
pub struct WorkspaceMenu {
    menu: ContextMenu,
    /// What each row of `menu` stands for, `None` for separators and the workspace the person is
    /// in.
    rows: Vec<Option<WorkspaceMenuChoice>>,
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
            MenuPlace::Bar => Self::switcher_rows(model),
            MenuPlace::Shell => Self::shell_rows(model),
            MenuPlace::Strips => Self::strips_rows(model),
        }
    }

    /// The strips menu: the workspace's strips, then a new one.
    fn strips_rows(model: &WorkspaceMenuModel) -> Vec<Row> {
        let mut rows: Vec<Row> = model.strips.iter().map(WorkspaceEntry::row).collect();
        if model.can_add_strip {
            if !rows.is_empty() {
                rows.push(Row::rule());
            }
            rows.push(Self::new_strip_row());
        }
        rows
    }

    fn new_strip_row() -> Row {
        Row::new(
            MenuItem::selection(t("workspace-menu-new-strip")).with_icon(Icon::Plus),
            Some(WorkspaceMenuChoice::NewStrip),
        )
    }

    /// The switcher: the person's own desktop, where they have one, then the workspaces, then the
    /// ways to another one.
    fn switcher_rows(model: &WorkspaceMenuModel) -> Vec<Row> {
        let mut rows: Vec<Row> = model.own_desktop.iter().map(WorkspaceAction::row).collect();
        if !rows.is_empty() && !model.workspaces.is_empty() {
            // The person's own desktop is no workspace: a rule says the list below is the others.
            rows.push(Row::rule());
        }
        rows.extend(model.workspaces.iter().map(WorkspaceEntry::row));
        if !rows.is_empty() && !model.actions.is_empty() {
            rows.push(Row::rule());
        }
        rows.extend(model.actions.iter().map(WorkspaceAction::row));
        if let Some(settings) = &model.settings {
            if !rows.is_empty() {
                rows.push(Row::rule());
            }
            rows.push(settings.row());
        }
        rows
    }

    /// What a press on the shell offers: the shell's own verbs, then a new strip.
    fn shell_rows(model: &WorkspaceMenuModel) -> Vec<Row> {
        let mut rows: Vec<Row> = model
            .shell
            .iter()
            .take(SHELL_ROWS)
            .map(WorkspaceAction::row)
            .collect();
        if model.can_add_strip {
            if !rows.is_empty() {
                rows.push(Row::rule());
            }
            rows.push(Self::new_strip_row());
        }
        rows
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

    /// Open the menu for `model` with its first row at `at` (a secondary press on the shell) within
    /// `viewport` , or close it when it is open.
    pub fn open_at(&mut self, model: &WorkspaceMenuModel, at: Point, viewport: Rect) {
        if self.menu.is_open() {
            self.close();
            return;
        }
        self.show(model, Rect::new(at.x, at.y, at.x, at.y), viewport);
    }

    /// Draw the rows `model` makes, anchored `anchor` , kept inside `viewport` (the [`ContextMenu`]
    /// flips it above or left at an edge). A menu with no rows does not open.
    fn show(&mut self, model: &WorkspaceMenuModel, anchor: Rect, viewport: Rect) {
        let (items, rows): (Vec<_>, Vec<_>) = Self::rows(model)
            .into_iter()
            .map(|row| (row.item, row.choice))
            .unzip();
        if items.is_empty() {
            return;
        }
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

    /// The label of the row marked as the workspace the person is in.
    pub fn marked(&self) -> Option<String> {
        self.menu
            .items()
            .iter()
            .find(|item| item.checked() == Some(true))
            .and_then(|item| item.label().map(str::to_owned))
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
        // Over the whole host, out of its flow, like the overview.
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
    use tilo_desktop_model::StripId;

    fn action(id: &str, title: &str) -> WorkspaceAction {
        WorkspaceAction {
            id: id.into(),
            title: title.into(),
            icon: Some(Icon::Plus),
            destructive: false,
        }
    }

    fn entry(name: &str, current: bool, choice: WorkspaceMenuChoice) -> WorkspaceEntry {
        WorkspaceEntry {
            name: name.into(),
            current,
            choice,
        }
    }

    fn model() -> WorkspaceMenuModel {
        WorkspaceMenuModel {
            place: MenuPlace::Bar,
            workspaces: vec![
                entry("Studio", true, WorkspaceMenuChoice::Switch("studio".into())),
                entry(
                    "Garden",
                    false,
                    WorkspaceMenuChoice::Switch("garden".into()),
                ),
                entry(
                    "Lounge",
                    false,
                    WorkspaceMenuChoice::Action("open-workspace:lounge".into()),
                ),
            ],
            own_desktop: Some(WorkspaceAction {
                id: "open-workspace:own".into(),
                title: "Open your desktop".into(),
                icon: Some(Icon::User),
                destructive: false,
            }),
            actions: vec![
                action("new-workspace", "New workspace"),
                action("join-workspace", "Join a workspace"),
            ],
            settings: Some(action("workspace-settings", "Workspace settings")),
            strips: vec![
                entry("Strip 1", true, WorkspaceMenuChoice::Strip(StripId(1))),
                entry("Research", false, WorkspaceMenuChoice::Strip(StripId(2))),
            ],
            // The switcher offers no strips, whatever the person may do.
            can_add_strip: true,
            ..WorkspaceMenuModel::default()
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

    fn shell_opened(model: &WorkspaceMenuModel) -> WorkspaceMenu {
        ui::i18n::I18nManager::init("en-US");
        let mut menu = WorkspaceMenu::new();
        menu.open_at(
            model,
            Point::new(640.0, 400.0),
            Rect::new(0.0, 0.0, 1280.0, 720.0),
        );
        menu
    }

    /// The menu a press on the shell opens: eight verbs, of which a menu's worth show.
    fn shell_model() -> WorkspaceMenuModel {
        WorkspaceMenuModel {
            place: MenuPlace::Shell,
            shell: (0..8)
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

    tilo_test_env::scenario! {
        fn the_switcher_lists_workspaces_and_the_shells_menu_its_verbs;

        phase the_switcher_holds_the_workspaces_then_new_and_join_and_nothing_else;
        {
            let menu = opened(&model());
            assert!(menu.is_open());
            assert_eq!(
                menu.labels(),
                [
                    Some("Open your desktop".to_owned()),
                    None,
                    Some("Studio".to_owned()),
                    Some("Garden".to_owned()),
                    Some("Lounge".to_owned()),
                    None,
                    Some("New workspace".to_owned()),
                    Some("Join a workspace".to_owned()),
                    None,
                    Some("Workspace settings".to_owned()),
                ],
                "the person's own desktop stands apart above their workspaces"
            );
            assert_eq!(menu.marked().as_deref(), Some("Studio"), "the one the person is in is marked");
        }

        phase the_persons_own_desktop_is_offered_apart_and_chosen_like_a_workspace;
        {
            let mut menu = opened(&model());
            assert!(menu.choose("Open your desktop"));
            assert_eq!(
                menu.take_choice(),
                Some(WorkspaceMenuChoice::Action("open-workspace:own".into()))
            );
            // Without one of their own (or in it, where no other place exists), the list is the
            // workspaces alone.
            let alone = opened(&WorkspaceMenuModel {
                own_desktop: None,
                ..model()
            });
            assert_eq!(alone.labels()[0], Some("Studio".to_owned()));
        }

        phase choosing_a_workspace_goes_there_once_and_closes_the_menu;
        {
            for (label, choice) in [
                ("Garden", WorkspaceMenuChoice::Switch("garden".into())),
                (
                    "Lounge",
                    WorkspaceMenuChoice::Action("open-workspace:lounge".into()),
                ),
                (
                    "New workspace",
                    WorkspaceMenuChoice::Action("new-workspace".into()),
                ),
                (
                    "Join a workspace",
                    WorkspaceMenuChoice::Action("join-workspace".into()),
                ),
            ] {
                let mut menu = opened(&model());
                assert!(menu.choose(label), "{label} is a row");
                assert_eq!(menu.take_choice(), Some(choice), "{label}");
                assert_eq!(menu.take_choice(), None, "{label} is said once");
                assert!(!menu.is_open(), "{label} closes the menu");
            }
        }

        phase choosing_the_workspace_the_person_is_in_changes_nothing;
        {
            let mut menu = opened(&model());
            assert!(menu.choose("Studio"));
            assert_eq!(menu.take_choice(), None);
        }

        phase toggling_again_closes;
        {
            let mut menu = opened(&model());
            menu.toggle(&model(), Rect::ZERO, Rect::new(0.0, 0.0, 800.0, 600.0));
            assert!(!menu.is_open());
        }

        phase the_strip_name_lists_the_strips_and_makes_a_new_one;
        {
            let strips = WorkspaceMenuModel {
                place: MenuPlace::Strips,
                ..model()
            };
            let mut menu = opened(&strips);
            assert_eq!(
                menu.labels(),
                [
                    Some("Strip 1".to_owned()),
                    Some("Research".to_owned()),
                    None,
                    Some("New strip".to_owned()),
                ],
                "the strips, then a new one; nothing about workspaces"
            );
            assert_eq!(menu.marked().as_deref(), Some("Strip 1"));
            assert!(menu.choose("Research"));
            assert_eq!(
                menu.take_choice(),
                Some(WorkspaceMenuChoice::Strip(StripId(2)))
            );
            let mut menu = opened(&strips);
            assert!(menu.choose("New strip"));
            assert_eq!(menu.take_choice(), Some(WorkspaceMenuChoice::NewStrip));
            let viewer = opened(&WorkspaceMenuModel {
                can_add_strip: false,
                ..strips
            });
            assert!(
                !viewer.labels().contains(&Some("New strip".to_owned())),
                "no new strip for someone who may not add one"
            );
        }

        phase a_press_on_the_shell_offers_its_verbs_and_a_new_strip;
        {
            let mut menu = shell_opened(&shell_model());
            let labels: Vec<String> = menu.labels().into_iter().flatten().collect();
            assert_eq!(
                labels,
                ["Verb 0", "Verb 1", "Verb 2", "Verb 3", "Verb 4", "Verb 5", "New strip"],
                "a menu's worth of the shell's verbs, then a new strip; no workspace in it"
            );
            assert!(menu.choose("New strip"));
            assert_eq!(menu.take_choice(), Some(WorkspaceMenuChoice::NewStrip));
            let viewer = shell_opened(&WorkspaceMenuModel {
                can_add_strip: false,
                ..shell_model()
            });
            assert!(
                !viewer.labels().contains(&Some("New strip".to_owned())),
                "no new strip for someone who may not add one"
            );
        }

        phase a_second_press_on_the_shell_closes_the_menu_it_opened;
        {
            let mut menu = shell_opened(&shell_model());
            assert!(menu.choose("Verb 0"));
            assert_eq!(
                menu.take_choice(),
                Some(WorkspaceMenuChoice::Action("launchpad-0".into()))
            );
            let shell = shell_model();
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            menu.open_at(&shell, Point::new(10.0, 10.0), viewport);
            assert!(menu.is_open(), "opening it again after choosing opens it");
            menu.open_at(&shell, Point::new(10.0, 10.0), viewport);
            assert!(!menu.is_open(), "a second press on the shell closes it");
        }
    }
}
