//! The parts the apps this desktop runs put in their windows' bars.
//! Every app here runs in the host's process, each on the window its tile is shown in
//! ([`ui::hosted_window::HostedWindow`], given as the tile is built): the desktop shows what each
//! declared in its tab's bar and hands it what the person did there, through the toolkit's
//! [`ui::window_parts::WindowParts`], as the session's shell does.
//! An app in a process of its own reaches the same bar over its `shell-ui` connection: the session
//! passes on what it asked ([`DesktopHandle::declare_bar`] and its siblings), and takes back what
//! the person did there for the app's connection ([`DesktopHandle::take_bar_events`]).

use super::{DesktopHandle, TabId};
use ui::window_targets::{TargetAction, WindowTarget};
use ui::window_toolbar::{SearchOpening, ToolbarEvent, WindowOffer, WindowToolbar};

/// What the apps in processes of their own asked of their windows' bars since the last update pass,
/// and what the person did there for them.
/// An app reaches its window's bar over its `shell-ui` connection ([`ui::window_parts`]): the
/// session passes on what it declared and what it offers, its asks to edit the location or open the
/// search, and takes back what the person did there, to write on the same connection.
#[derive(Default)]
pub struct Bars {
    /// Parts declared for a tab, by the connection that declared them.
    pub declared: Vec<(u64, u64, Option<WindowToolbar>)>,
    /// What a tab's window offers the bar's search and History control.
    pub offered: Vec<(u64, u64, WindowOffer)>,
    /// Tabs whose app asked for its location to be edited.
    pub edits: Vec<u64>,
    /// Tabs a person asked to name in their window's bar.
    pub renames: Vec<u64>,
    /// A field in a window's bar takes what is typed (a tab being named, a location edited): the
    /// shell holds the keyboard until it closes, so the keys do not reach the app under it.
    pub typing: bool,
    /// Tabs whose app asked for its search, and how it opens.
    pub searches: Vec<(u64, SearchOpening)>,
    /// Connections that closed: what they declared goes with them.
    pub gone: Vec<u64>,
    /// What the person did in the bars, by the tab whose window it was for and its connection,
    /// waiting for the session to write it to the app.
    pub events: Vec<(u64, u64, ToolbarEvent)>,
    /// The name each app gives what its window shows (its bar's location: a note's title, a
    /// folder's path), by tab, as the bar last read it. A window's name is what it shows, so it
    /// stands in front of the window's own title everywhere the tab is named.
    pub names: std::collections::BTreeMap<u64, String>,
    /// The controls each window's app describes for its own content, by tab,
    /// with the connection that declared them.
    pub targets: std::collections::BTreeMap<u64, (u64, Vec<WindowTarget>)>,
    /// What the apps in processes of their own declared of their windows' content since the last
    /// update pass.
    pub declared_targets: Vec<(u64, u64, Vec<WindowTarget>)>,
    /// The controls the person pressed in the windows' content, by connection, waiting for the
    /// session to write them to the app.
    pub presses: Vec<TargetPress>,
    /// The parts each window's app declared for its bar, by tab, with the connection that declared
    /// them: what names the window ([`Self::names`]) and what the shell's bar reads.
    pub toolbars: std::collections::BTreeMap<u64, (u64, WindowToolbar)>,
    /// The field each window's bar holds open, by tab: the person typing a name, a location, or a
    /// search the app asked for.
    pub open: std::collections::BTreeMap<u64, BarField>,
    /// What each window offers the bar's search and History control, by tab, with the connection
    /// that said it.
    pub offers: std::collections::BTreeMap<u64, (u64, WindowOffer)>,
}

/// A control of a window's content the person pressed, as the session writes it to the app: the
/// connection that declared it, its name and role, and what it does when the person named it by
/// that instead of its name — the one a test, an agent or assistive technology looks up without
/// reading the person's language.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPress {
    pub connection: u64,
    /// The tab whose window shows the control: where the input before the press went, so the
    /// press comes after it.
    pub tab: u64,
    pub name: String,
    pub role: String,
    pub identity: Option<String>,
    pub action: TargetAction,
}

/// The field a window's bar holds open: the person naming the tab, editing the app's location, or
/// searching what the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarField {
    /// The app's own name for what the window shows, editable as its Ctrl+L asks.
    Location,
    /// The tab's name, opened by its menu's Rename or a double click on its tab.
    Rename,
    /// The window's search, as the app's Ctrl+F or Ctrl+H asks it to open.
    Search(SearchOpening),
}

impl Bars {
    /// Take `names` as what the windows' apps call what they show; whether that changed anything.
    /// The bar reads them from the parts the apps declared, so an unchanged pass costs one
    /// comparison.
    pub fn take_names(&mut self, names: std::collections::BTreeMap<u64, String>) -> bool {
        if self.names == names {
            return false;
        }
        self.names = names;
        true
    }
}

/// Name each window by what it shows: its bar's location label, a note's title or a folder's path.
/// A label that names a part of the app ([`ui::window_toolbar::ToolbarLocation::section`]) leaves
/// the window named by its app. A window's name is what it shows, so it is written into the tab:
/// the bar, the overview and search all read the tab's name, and the window's own title only
/// stands where its app names nothing. Whether that changed anything.
fn name_windows(shared: &mut super::Shared) -> bool {
    let mut names = std::collections::BTreeMap::new();
    // The windows whose bar names a part of their app ("Issues", "Presets"): where the window is,
    // not what it shows, so the window goes by its app's name.
    let mut sections = std::collections::BTreeSet::new();
    for (tab, (_, toolbar)) in &shared.bars.toolbars {
        let Some(location) = toolbar.location.as_ref() else {
            continue;
        };
        let label = location.label.trim();
        if location.section {
            sections.insert(*tab);
        } else if !label.is_empty() {
            names.insert(*tab, label.to_owned());
        }
    }
    let named_before = shared.bars.names.clone();
    // What the bars name is read when a window is titled, never drawn again for: a name the bars
    // changed, once a key while a note's first line is typed, wakes nothing.
    let changed = shared.bars.take_names(names.clone());
    if changed {
        shared.note_change();
    }
    let mut titled = false;
    for tab in shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
        .flat_map(|strip| strip.window_groups.iter_mut())
        .flat_map(|group| group.tabs.iter_mut())
    {
        // A tab that shows one thing of the app is the person's place of that thing: it names
        // itself by what it shows, and no window's name is written into it.
        if tilo_desktop_model::apps::shows_a_view(tab) {
            continue;
        }
        if let Some(name) = names.get(&tab.id) {
            titled |= tab.set_window_title(name);
        } else if sections.contains(&tab.id)
            && let Some(app) = super::app_name_of(tab)
        {
            titled |= tab.set_window_title(&app);
        } else if named_before
            .get(&tab.id)
            .is_some_and(|name| tab.title == *name)
            && let Some(app) = super::app_name_of(tab)
        {
            // The app names nothing any more (the note it showed is closed), so the window goes
            // back to its app's name rather than keeping a place it has left.
            titled |= tab.set_window_title(&app);
        }
    }
    if titled {
        // A window's name is what a peer reads of the shell, so a name that changed is a change of
        // it: the page publishes the new name.
        shared.note_change();
    }
    changed | titled
}

impl DesktopHandle {
    /// Take up what the apps in processes of their own asked of their windows' bars since the last
    /// pass: their parts, what their windows offer and describe of their content, and the
    /// connections that closed; then name each window by what it shows. Asks for the bar's own
    /// controls (a location edited, a search or a name field opened) go to whatever draws the bar,
    /// and none is held for a bar nobody draws. Whether the desktop changed.
    pub fn follow_bars(&self) -> bool {
        let mut shared = self.shared.borrow_mut();
        let tabs: std::collections::BTreeSet<u64> = shared.tree.tabs().map(|tab| tab.id).collect();
        let bars = &mut shared.bars;
        let declared = std::mem::take(&mut bars.declared);
        let offered = std::mem::take(&mut bars.offered);
        let described = std::mem::take(&mut bars.declared_targets);
        let gone = std::mem::take(&mut bars.gone);
        // The bar's own controls the apps asked for open as fields the shell draws, and stay open
        // until the person closes them.
        let edits = std::mem::take(&mut bars.edits);
        let searches = std::mem::take(&mut bars.searches);
        let renames = std::mem::take(&mut bars.renames);
        let before = bars.open.clone();
        bars.open.retain(|tab, _| tabs.contains(tab));
        for tab in edits {
            bars.open.insert(tab, BarField::Location);
        }
        for tab in renames {
            bars.open.insert(tab, BarField::Rename);
        }
        for (tab, opening) in searches {
            bars.open.insert(tab, BarField::Search(opening));
        }
        let mut changed = before != bars.open;
        let typing = !bars.open.is_empty();
        if bars.typing != typing {
            bars.typing = typing;
            changed = true;
        }
        for connection in gone {
            // What its app declared and described of a window goes with it.
            let before = (bars.targets.len(), bars.toolbars.len(), bars.offers.len());
            bars.targets.retain(|_, (owner, _)| *owner != connection);
            bars.toolbars.retain(|_, (owner, _)| *owner != connection);
            bars.offers.retain(|_, (owner, _)| *owner != connection);
            changed |= before != (bars.targets.len(), bars.toolbars.len(), bars.offers.len());
        }
        for (tab, connection, toolbar) in declared {
            changed |= match toolbar {
                Some(toolbar) => {
                    let held = (connection, toolbar);
                    let new = bars.toolbars.get(&tab) != Some(&held);
                    bars.toolbars.insert(tab, held);
                    new
                }
                None => bars.toolbars.remove(&tab).is_some(),
            };
        }
        for (tab, connection, offer) in offered {
            bars.offers.insert(tab, (connection, offer));
        }
        let mut offered_by_name = false;
        for (tab, connection, targets) in described {
            if bars
                .targets
                .get(&tab)
                .is_some_and(|(owner, held)| *owner == connection && *held == targets)
            {
                continue;
            }
            bars.targets.insert(tab, (connection, targets));
            offered_by_name = true;
        }
        if offered_by_name {
            // What a window offers by name is part of what the shell publishes.
            shared.changed();
            changed = true;
        }
        changed | name_windows(&mut shared)
    }

    /// The name `tab`'s app gives what its window shows (its bar's location:
    /// a note's title, a folder's path), as the bar last read it. A window's
    /// name is what it shows, so this stands in front of the window's own
    /// title ([`tilo_desktop_policy::DesktopPolicy::retitle`],
    /// [`DesktopHandle::declare_bar`]).
    #[must_use]
    pub fn window_name(&self, tab: u64) -> Option<String> {
        self.shared.borrow().bars.names.get(&tab).cloned()
    }

    /// `connection` 's app declared `toolbar` for `tab` , or none. Shown from the next pass on. The
    /// app is the one the session vouched for, so it only ever reaches a tab that shows it.
    pub fn declare_bar(&self, tab: u64, connection: u64, toolbar: Option<WindowToolbar>) {
        self.queue_bar_ask(|bars| bars.declared.push((tab, connection, toolbar)));
    }

    /// `connection` 's app said what `tab` 's window offers the bar's search and History control.
    pub fn offer_bar(&self, tab: u64, connection: u64, offer: WindowOffer) {
        self.queue_bar_ask(|bars| bars.offered.push((tab, connection, offer)));
    }

    /// `tab` 's app asked for its location in the bar to be edited, as its own Ctrl+L does.
    pub fn edit_bar_location(&self, tab: u64) {
        self.queue_bar_ask(|bars| bars.edits.push(tab));
    }

    /// Open `tab`'s name field in its window's bar, as its menu's Rename and a double click on the
    /// tab do: the field holds the keyboard, Enter names the tab and Escape leaves it as it was.
    pub fn rename_in_bar(&self, tab: u64) {
        self.queue_bar_ask(|bars| bars.renames.push(tab));
    }

    /// Whether a field in a window's bar takes what is typed now.
    pub fn bar_takes_typing(&self) -> bool {
        self.shared.borrow().bars.typing
    }
    /// The parts the window `tab`'s app declared for its bar, as the shell's views draw them:
    /// its leading controls, actions, views, notice and reactions.
    #[must_use]
    pub fn bar_toolbar(&self, tab: TabId) -> Option<WindowToolbar> {
        self.shared
            .borrow()
            .bars
            .toolbars
            .get(&tab)
            .map(|(_, toolbar)| toolbar.clone())
    }

    /// The field the window `tab`'s bar holds open, as the shell's views draw it.
    #[must_use]
    pub fn bar_field(&self, tab: TabId) -> Option<BarField> {
        self.shared.borrow().bars.open.get(&tab).copied()
    }

    /// Close the field the window `tab`'s bar held open, as Escape or its Done does.
    pub fn close_bar_field(&self, tab: TabId) {
        self.queue_bar_ask(|bars| {
            bars.open.remove(&tab);
            bars.typing = !bars.open.is_empty();
        });
    }

    /// Open the field that names the window `tab`: what its menu's Rename and a double click on
    /// its tab ask for.
    pub fn rename_in_bar_now(&self, tab: TabId) {
        self.queue_bar_ask(|bars| {
            bars.open.insert(tab, BarField::Rename);
            bars.typing = true;
        });
    }

    /// What the person did in the field the window `tab`'s bar holds open, for the app that asked
    /// for it: written to its connection by the next pass. A field nothing else asked for (the
    /// tab's own name) is the session's to act on, and sends nothing.
    pub fn bar_event(&self, tab: TabId, event: ToolbarEvent) {
        let connection = self
            .shared
            .borrow()
            .bars
            .toolbars
            .get(&tab)
            .map(|(connection, _)| *connection);
        let Some(connection) = connection else {
            return;
        };
        self.queue_bar_ask(|bars| bars.events.push((tab, connection, event)));
    }


    /// `tab` 's app asked for its window's search in the bar to open, as its own Ctrl+F (or Ctrl+H,
    /// for `opening` ) does.
    pub fn open_bar_search(&self, tab: u64, opening: SearchOpening) {
        self.queue_bar_ask(|bars| bars.searches.push((tab, opening)));
    }

    /// Queue shell work an app handed over, and ask the shell for its next pass: what an app asks
    /// reaches its window's bar in the pass that follows, not whenever the shell next draws
    /// something of its own.
    fn queue_bar_ask(&self, ask: impl FnOnce(&mut Bars)) {
        let mut shared = self.shared.borrow_mut();
        ask(&mut shared.bars);
        shared.changed();
    }

    /// `connection` 's app described the controls of `tab` 's content ([`ui::window_targets`]):
    /// what a session finds and presses there.
    pub fn declare_targets(&self, tab: u64, connection: u64, targets: Vec<WindowTarget>) {
        self.queue_bar_ask(|bars| bars.declared_targets.push((tab, connection, targets)));
    }

    /// The controls the windows the shell shows offer by name and by what they do now, each with
    /// where it stands on the shell: where a test, a tool or an agent finds them. A window's
    /// controls are offered only while it is drawn, and its app's coordinates are offset by the
    /// tab's content, so where a control is and where the person sees it agree.
    #[must_use]
    pub fn window_controls(&self) -> Vec<ui::window_targets::WindowControl> {
        let shared = self.shared.borrow();
        let mut shown = Vec::new();
        for (tab, content) in shared.shown_tabs.content() {
            let Some((_, targets)) = shared.bars.targets.get(&tab) else {
                continue;
            };
            for target in targets {
                shown.push(ui::window_targets::WindowControl {
                    name: target.name.clone(),
                    identity: target.identity.clone(),
                    rect: (
                        content.x0 + target.rect[0],
                        content.y0 + target.rect[1],
                        target.rect[2] - target.rect[0],
                        target.rect[3] - target.rect[1],
                    ),
                    level: target.level,
                    selected: target.selected,
                });
            }
        }
        shown
    }

    /// Press the control `name` of `tab`'s window: the one *that* window shows, whichever other
    /// window goes by the same name. What a person pointing at the window in front presses.
    ///
    /// # Errors
    /// `tab`'s window is not drawn, shows no control by that name, or it answers nothing of
    /// `action`.
    pub fn press_window_control_in(
        &self,
        tab: TabId,
        name: &str,
        action: TargetAction,
    ) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        let Some((connection, targets)) = shared.bars.targets.get(&tab) else {
            return Err(format!("no window {tab} shows a control named {name}"));
        };
        let Some(target) = targets
            .iter()
            .find(|target| target.name == name && target.actions.contains(&action))
        else {
            return Err(format!("window {tab} shows no control named {name}"));
        };
        let press = TargetPress {
            connection: *connection,
            tab,
            name: target.name.clone(),
            role: target.role.clone(),
            identity: None,
            action,
        };
        shared.bars.presses.push(press);
        shared.changed();
        Ok(())
    }

    /// The window a press by name reaches first: the active window of the strip the person works
    /// on, whose content is what stands in front of them. `None` while the shell shows none.
    #[must_use]
    pub fn window_in_front(&self) -> Option<TabId> {
        let shared = self.shared.borrow();
        let tree = &shared.tree;
        let strip_id = tree.active_strip_id()?;
        let group_id = tree.active_group_for(strip_id)?;
        let workspace = tree.active_workspace()?;
        let strip = workspace.strips.iter().find(|strip| strip.id == strip_id)?;
        let group = strip
            .window_groups
            .iter()
            .find(|group| group.id == group_id)?;
        Some(group.active_tab)
    }

    /// Whether the app of `tab` described a control named `name` for its window's content.
    #[must_use]
    pub fn window_offers(&self, tab: TabId, name: &str) -> bool {
        let shared = self.shared.borrow();
        shared
            .bars
            .targets
            .get(&tab)
            .is_some_and(|(_, targets)| targets.iter().any(|target| target.name == name))
    }

    /// Press the control `name` of a window the shell shows, as an assistive
    /// technology does: the app acts on it through its own accessibility
    /// path and answers on the connection that declared it. The first window
    /// of the strip that shows the name answers, and the press names its role
    /// too, so the app finds the control it described and not another that
    /// goes by the same name.
    ///
    /// # Errors
    /// No window the shell draws shows a control by that name, or it answers
    /// nothing of `action`.
    pub fn press_window_control(&self, name: &str, action: TargetAction) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        for (tab, _) in shared.shown_tabs.content() {
            let Some((connection, targets)) = shared.bars.targets.get(&tab) else {
                continue;
            };
            let Some(target) = targets
                .iter()
                .find(|target| target.name == name && target.actions.contains(&action))
            else {
                continue;
            };
            let press = TargetPress {
                connection: *connection,
                tab,
                name: target.name.clone(),
                role: target.role.clone(),
                identity: None,
                action,
            };
            shared.bars.presses.push(press);
            shared.changed();
            return Ok(());
        }
        Err(format!("no window shows a control named {name}"))
    }

    /// Press the control that does `identity` in a window the shell shows, as an assistive
    /// technology does: the app acts on it through its own accessibility path and answers on the
    /// connection that declared it. What the control does is where the window composes it and
    /// what the person's language calls it, so this finds the control a name cannot.
    ///
    /// # Errors
    /// No window the shell draws shows a control doing that, or it answers nothing of `action`.
    pub fn press_window_control_doing(
        &self,
        identity: &str,
        action: TargetAction,
    ) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        for (tab, _) in shared.shown_tabs.content() {
            let Some((connection, targets)) = shared.bars.targets.get(&tab) else {
                continue;
            };
            let Some(target) = targets.iter().find(|target| {
                target.identity.as_deref() == Some(identity) && target.actions.contains(&action)
            }) else {
                continue;
            };
            let press = TargetPress {
                connection: *connection,
                tab,
                name: target.name.clone(),
                role: target.role.clone(),
                identity: target.identity.clone(),
                action,
            };
            shared.bars.presses.push(press);
            shared.changed();
            return Ok(());
        }
        Err(format!("no window shows a control doing {identity}"))
    }

    /// Whether the app of `tab` described a control doing `identity` for its window's content.
    #[must_use]
    pub fn window_does(&self, tab: TabId, identity: &str) -> bool {
        let shared = self.shared.borrow();
        shared.bars.targets.get(&tab).is_some_and(|(_, targets)| {
            targets
                .iter()
                .any(|target| target.identity.as_deref() == Some(identity))
        })
    }

    /// What the person, a tool or an agent pressed in the windows' content since the last call, by
    /// connection: what the session writes to each app.
    pub fn take_target_presses(&self) -> Vec<TargetPress> {
        std::mem::take(&mut self.shared.borrow_mut().bars.presses)
    }

    /// `connection` closed: the parts and offers its app declared go with it.
    pub fn bar_connection_closed(&self, connection: u64) {
        self.queue_bar_ask(|bars| bars.gone.push(connection));
    }

    /// What the person did in the bars for the apps in processes of their own since the last call,
    /// by the tab whose window it was for and its connection.
    pub fn take_bar_events(&self) -> Vec<(u64, u64, ToolbarEvent)> {
        std::mem::take(&mut self.shared.borrow_mut().bars.events)
    }
}

impl DesktopHandle {
    /// Name the agent run that opened each window an agent opened for its person, by tab, as the
    /// workspace's shared layout says: the window's bar names the agent, and opens the run in History.
    /// Whether that changed anything.
    pub fn set_opened_by(
        &self,
        opened_by: std::collections::BTreeMap<u64, tilo_desktop_model::ownership::OpenedBy>,
    ) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.opened_by == opened_by {
            return false;
        }
        shared.opened_by = opened_by;
        shared.changed();
        true
    }

    /// Name the person each agent that opened a window opened it for, by tab, as its server knows
    /// them: pointing at the agent's mark in the window's bar says whom it works for. Whether that
    /// changed anything.
    pub fn set_opened_for(&self, opened_for: std::collections::BTreeMap<u64, String>) -> bool {
        let mut shared = self.shared.borrow_mut();
        if shared.opened_for == opened_for {
            return false;
        }
        shared.opened_for = opened_for;
        shared.changed();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::seed_tree;
    use crate::desktop::state::DesktopHandle;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};
    use ui::window_toolbar::ToolbarAction;

    /// A shell of one window running an app of its own.
    fn shell() -> DesktopHandle {
        let mut minter = IdMinter::new(7);
        let tab = Tab {
            id: minter.mint(),
            title: "App0".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.app0".into(),
                label: "App0".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = seed_tree(&mut minter, "Office", vec![tab]);
        DesktopHandle::new(tree, 7)
    }

    /// The fields the apps and the person ask for stay open across the shell's own passes, and
    /// the shell reads which one each window's bar holds, with the keys held off the window while
    /// one is: a search the app asked for is not lost to the next frame.
    #[test]
    fn a_bar_field_stays_open_until_it_is_closed() {
        let handle = shell();
        handle.open_bar_search(7, ui::window_toolbar::SearchOpening::Replace);
        handle.follow_bars();
        assert_eq!(
            handle.bar_field(7),
            Some(BarField::Search(ui::window_toolbar::SearchOpening::Replace))
        );
        assert!(handle.bar_takes_typing(), "the field takes what is typed");
        handle.follow_bars();
        assert_eq!(
            handle.bar_field(7),
            Some(BarField::Search(ui::window_toolbar::SearchOpening::Replace)),
            "a pass of the shell's own leaves the field open"
        );
        handle.close_bar_field(7);
        handle.follow_bars();
        assert_eq!(handle.bar_field(7), None);
        assert!(!handle.bar_takes_typing());

        handle.rename_in_bar_now(7);
        handle.follow_bars();
        assert_eq!(handle.bar_field(7), Some(BarField::Rename));
        handle.rename_in_bar(7);
        handle.follow_bars();
        assert_eq!(handle.bar_field(7), Some(BarField::Rename));
        handle.edit_bar_location(7);
        handle.follow_bars();
        assert_eq!(handle.bar_field(7), Some(BarField::Location));
    }

    /// The parts a window's app declares reach the shell as they were declared, and a press on
    /// one goes back to that app as the action it names — a leading control, an action, a view,
    /// a notice's button and a reaction alike.
    #[test]
    fn a_declared_bar_reaches_the_shell_and_its_presses_go_back() {
        use ui::window_toolbar::{
            ToolbarNavigation, ToolbarReaction, ToolbarReactions, ToolbarView, ToolbarViews,
        };
        let handle = shell();
        let toolbar = WindowToolbar {
            navigation: Some(ToolbarNavigation {
                back: true,
                forward: false,
            }),
            sidebar: Some(true),
            leading: vec![ToolbarAction::new("home", "house", "Home")],
            actions: vec![ToolbarAction::new("save", "check", "Save")],
            views: Some(ToolbarViews {
                name: "View".into(),
                choices: vec![ToolbarView {
                    id: "list".into(),
                    label: "List".into(),
                }],
                chosen: "list".into(),
            }),
            reactions: Some(ToolbarReactions {
                pills: vec![ToolbarReaction {
                    emoji: "🎉".into(),
                    count: 2,
                    mine: false,
                    names: vec!["Ada".into()],
                }],
                can_react: true,
            }),
            ..WindowToolbar::default()
        };
        handle.declare_bar(7, 41, Some(toolbar));
        handle.follow_bars();
        let held = handle.bar_toolbar(7).expect("the bar is declared");
        assert!(held.navigation.is_some_and(|navigation| navigation.back));
        assert_eq!(held.leading.len(), 1);
        assert_eq!(held.actions.len(), 1);
        assert_eq!(held.sidebar, Some(true));
        handle.bar_event(
            7,
            ToolbarEvent::Action {
                id: "save".into(),
            },
        );
        assert_eq!(
            handle.take_bar_events(),
            vec![(
                7,
                41,
                ToolbarEvent::Action {
                    id: "save".into()
                }
            )],
            "a press goes back to the app that declared the bar"
        );
    }

    /// What the person does in a bar field goes to the app that declared the bar, and to no
    /// other: a window whose app keeps its bar to itself sends nothing.
    #[test]
    fn bar_events_go_to_the_app_that_declared_the_bar() {
        let handle = shell();
        handle.bar_event(
            7,
            ToolbarEvent::Search {
                query: "x".into(),
            },
        );
        assert!(
            handle.take_bar_events().is_empty(),
            "no app declared a bar to hear it"
        );
        handle.declare_bar(7, 41, Some(WindowToolbar::default()));
        handle.follow_bars();
        handle.bar_event(
            7,
            ToolbarEvent::Search {
                query: "y".into(),
            },
        );
        assert_eq!(
            handle.take_bar_events(),
            vec![(
                7,
                41,
                ToolbarEvent::Search {
                    query: "y".into()
                }
            )]
        );
    }

    /// What an app in a process of its own asks of its window's bar is shell work
    /// the shell is asked to take up: each one brings it the pass that shows it,
    /// so a bar the person is not touching shows what its app declared rather
    /// than waiting for the shell to draw something of its own.
    #[test]
    fn every_bar_ask_brings_the_shell_its_next_pass() {
        let handle = shell();
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let woken = std::sync::Arc::clone(&asked);
        handle.set_wake(std::sync::Arc::new(move || {
            woken.store(true, std::sync::atomic::Ordering::Relaxed);
        }));
        let toolbar = WindowToolbar {
            actions: vec![ToolbarAction::new("save", "plus", "Save")],
            ..WindowToolbar::default()
        };

        handle.declare_bar(7, 41, Some(toolbar));
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "a declaration asks for its pass"
        );
        handle.offer_bar(7, 41, ui::window_toolbar::WindowOffer::default());
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "an offer asks for its pass"
        );
        handle.declare_bar(7, 41, None);
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "taking parts away asks for its pass"
        );
        handle.edit_bar_location(7);
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "editing the location asks for its pass"
        );
        handle.open_bar_search(7, ui::window_toolbar::SearchOpening::Find);
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "opening the search asks for its pass"
        );
        handle.declare_targets(7, 41, Vec::new());
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "the window's controls ask for their pass"
        );
        handle.bar_connection_closed(41);
        assert!(
            asked.swap(false, std::sync::atomic::Ordering::Relaxed),
            "dropping what a closed connection asked for asks for its pass"
        );
    }

    /// The names the bars read are kept as they last read them: an unchanged pass costs one
    /// comparison, and a window's name changes only when what it shows does.
    #[test]
    fn the_bars_keep_the_names_they_last_read() {
        let mut bars = Bars::default();
        assert!(
            !bars.take_names(std::collections::BTreeMap::new()),
            "nothing named yet"
        );
        let names = std::collections::BTreeMap::from([(7, "Launch notes".to_owned())]);
        assert!(bars.take_names(names.clone()), "a new name is a change");
        assert_eq!(bars.names, names);
        assert!(!bars.take_names(names), "the same names change nothing");
    }
}
