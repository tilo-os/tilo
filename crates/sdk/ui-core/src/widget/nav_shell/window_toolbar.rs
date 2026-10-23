//! The shell's own buttons as slots in the window's bar
//! ([`NavShell::with_window_toolbar`]), and the slots the shell declares
//! itself from its headers for an app that declares none ([`ShellBar`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{
    Column, InspectorPlacement, NavPane, NavShell, Part, SidebarPlacement, Staged, Widget, at,
    is_app_search,
};
use crate::window_toolbar::{ToolbarAction, ToolbarEvent, ToolbarLocation, WindowToolbar};

/// The shell's buttons, and an app inside it ([`WindowBarLink`]), report
/// presses here; the shell runs them at its next sync or update.
#[derive(Default)]
pub(super) struct Requests {
    pub(super) toggle: Cell<bool>,
    pub(super) back: Cell<bool>,
    pub(super) inspector: Cell<bool>,
    /// The notice's button was pressed in the bar.
    pub(super) notice_action: Cell<bool>,
    /// The sidebar open or closed, landed with no motion: a capture's
    /// staging ([`WindowBarLink::stage_sidebar`]).
    pub(super) staged_sidebar: Cell<Option<bool>>,
    /// The push stack's front screen, landed with no motion
    /// ([`WindowBarLink::stage_pane`]).
    pub(super) staged_pane: Cell<Option<NavPane>>,
    /// Put a floating sidebar away ([`WindowBarLink::put_drawer_away`]).
    pub(super) put_drawer_away: Cell<bool>,
    /// The shell's own slots for the window's bar as of its last layout.
    slots: RefCell<WindowToolbar>,
}

/// The shell's own buttons in the window's bar, for an app that paints
/// inside the shell's content column and declares the bar itself: it
/// starts its declaration from [`Self::slots`] (the sidebar toggle, Back
/// while it goes anywhere, the inspector's toggle) and hands what the
/// person did in the bar to [`Self::event`] first, as an app that owns its
/// shell does with [`NavShell::window_toolbar`] and
/// [`NavShell::window_toolbar_event`].
#[derive(Clone)]
pub struct WindowBarLink(Rc<Requests>);

impl WindowBarLink {
    /// The shell's own slots, as it last laid out.
    #[must_use]
    pub fn slots(&self) -> WindowToolbar {
        self.0.slots.borrow().clone()
    }

    /// Whether the shell shows its sidebar, as it last laid out.
    #[must_use]
    pub fn is_sidebar_open(&self) -> bool {
        self.0.slots.borrow().sidebar == Some(true)
    }

    /// Show the sidebar open or closed at once, with no motion: what a
    /// screenshot scenario stages.
    pub fn stage_sidebar(&self, open: bool) {
        self.0.staged_sidebar.set(Some(open));
    }

    /// Show `pane` in front at once, with no motion, when the window shows
    /// one column at a time: what a screenshot scenario of an opened item
    /// stages. A docked layout shows every column already.
    pub fn stage_pane(&self, pane: NavPane) {
        self.0.staged_pane.set(Some(pane));
    }

    /// Put the sidebar away if it floats as a drawer over the content (the
    /// app opened something the drawer would cover); a docked sidebar
    /// stays.
    pub fn put_drawer_away(&self) {
        self.0.put_drawer_away.set(true);
    }

    /// Run what the person did in the window's bar on the shell's own
    /// slots. Returns whether it was one of them; the app handles the rest.
    pub fn event(&self, event: &ToolbarEvent) -> bool {
        let request = match event {
            ToolbarEvent::ToggleSidebar => &self.0.toggle,
            ToolbarEvent::Back => &self.0.back,
            ToolbarEvent::Action { id } if id == WINDOW_BACK => &self.0.back,
            ToolbarEvent::Action { id } if id == WINDOW_INSPECTOR => &self.0.inspector,
            ToolbarEvent::Action { id } if id == super::notices::WINDOW_NOTICE_ACTION => {
                &self.0.notice_action
            }
            _ => return false,
        };
        request.set(true);
        true
    }
}

/// The action ids of the shell's own slots in the window's bar
/// ([`NavShell::window_toolbar`]).
pub const WINDOW_BACK: &str = "nav-shell-back";
pub const WINDOW_INSPECTOR: &str = "nav-shell-inspector";

impl NavShell {
    /// Run what the shell's buttons and a linked app asked for since the
    /// last pass. Returns whether anything changed.
    pub(super) fn take_requests(&mut self) -> bool {
        let mut acted = self.requests.toggle.replace(false);
        if acted {
            self.toggle_sidebar();
        }
        if self.requests.back.replace(false) {
            acted |= self.back();
        }
        if self.requests.inspector.replace(false) {
            self.toggle_inspector();
            acted = true;
        }
        if self.requests.notice_action.replace(false) {
            acted |= self.press_notice_action();
        }
        if self.built
            && let Some(open) = self.requests.staged_sidebar.take()
        {
            if self.geometry.sidebar == SidebarPlacement::Screen {
                self.show(if open {
                    NavPane::Sidebar
                } else {
                    NavPane::Content
                });
            } else if self.is_sidebar_open() != open {
                self.toggle_sidebar();
            }
            self.snap();
            acted = true;
        }
        if self.built
            && let Some(pane) = self.requests.staged_pane.take()
        {
            self.show(pane);
            self.snap();
            acted = true;
        }
        if self.requests.put_drawer_away.take()
            && self.geometry.sidebar == SidebarPlacement::Drawer
            && self.is_sidebar_open()
        {
            self.toggle_sidebar();
            acted = true;
        }
        acted
    }

    /// The app's toolbar is its window bar's slots ([`crate::window_toolbar`]):
    /// the shell draws no header row of its own, in a Tilo session or out of
    /// it. Its own buttons (the sidebar toggle, Back, the inspector toggle)
    /// are slots too: the app starts its declaration from
    /// [`Self::window_toolbar`] and hands what the person did in the bar to
    /// [`Self::window_toolbar_event`] first. Column titles and header
    /// actions given to the shell are not shown: the app declares its
    /// location and actions as slots.
    #[must_use]
    pub fn with_window_toolbar(self) -> Self {
        {
            let mut inputs = self.inputs.borrow_mut();
            inputs.toolbar_in_bar = true;
            inputs.toggle_in_bar = true;
        }
        self
    }

    /// The app shows the sidebar toggle in the window's bar
    /// ([`crate::window_toolbar::WindowToolbar::sidebar`]) and runs it with
    /// [`Self::toggle_sidebar`]: the header row keeps no toggle of its own,
    /// and stands only for what else it holds.
    pub fn set_sidebar_toggle_in_window_bar(&mut self, on: bool) {
        let changed = {
            let mut inputs = self.inputs.borrow_mut();
            std::mem::replace(&mut inputs.toggle_in_bar, on) != on
        };
        if changed {
            self.structure_dirty = true;
            self.invalidate();
        }
    }

    /// Put the header row in the window's bar: while the window is
    /// maximized, the row hides with the window's controls and appears
    /// with them over the top of the content when the person reaches for
    /// the bar (`widget::top_edge_reveal`), as one bar. In a tiled window
    /// the row stands above the content as usual.
    #[must_use]
    pub fn with_header_in_window_bar(self) -> Self {
        self.inputs.borrow_mut().header_in_bar = true;
        self
    }

    /// Whether Back goes anywhere: the app's page stack
    /// ([`Self::with_back`], [`Self::set_can_go_back`]) or a narrow
    /// window's push stack.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        self.geometry.back_column.is_some()
    }

    /// The shell's own slots for the window's bar
    /// ([`Self::with_window_toolbar`]): the sidebar toggle while there is a
    /// sidebar, Back while it goes anywhere (leading), and the inspector's
    /// toggle while there is an inspector (the last action). The app adds
    /// its location, views and actions; the inspector's toggle stays last.
    #[must_use]
    pub fn window_toolbar(&self) -> WindowToolbar {
        use crate::window_toolbar::ToolbarAction;
        let lookup = crate::i18n::I18nManager::lookup_plain;
        let mut toolbar = WindowToolbar {
            sidebar: self.has_sidebar().then(|| self.is_sidebar_open()),
            ..WindowToolbar::default()
        };
        if self.can_go_back() {
            toolbar.leading.push(ToolbarAction::new(
                WINDOW_BACK,
                "arrow-left",
                lookup("window-toolbar-back"),
            ));
        }
        if self.geometry.inspector != InspectorPlacement::None {
            let open = self.is_inspector_open();
            toolbar.actions.push(
                ToolbarAction::new(
                    WINDOW_INSPECTOR,
                    "panel-right",
                    lookup(if open {
                        "window-toolbar-hide-inspector"
                    } else {
                        "window-toolbar-show-inspector"
                    }),
                )
                .on(open),
            );
        }
        toolbar
    }

    /// The link an app painting inside the content column uses to put the
    /// shell's own buttons in the window's bar ([`WindowBarLink`]). Use with
    /// [`Self::with_window_toolbar`].
    #[must_use]
    pub fn window_bar_link(&self) -> WindowBarLink {
        WindowBarLink(Rc::clone(&self.requests))
    }

    /// Keep the shell's own slots where its [`WindowBarLink`] reads them.
    pub(super) fn publish_window_slots(&self) {
        let slots = self.window_toolbar();
        if *self.requests.slots.borrow() != slots {
            *self.requests.slots.borrow_mut() = slots;
        }
    }

    /// Add the app's own actions to [`Self::window_toolbar`]'s, before the
    /// inspector's toggle, which stays the last of the row.
    pub fn push_window_actions(
        toolbar: &mut crate::window_toolbar::WindowToolbar,
        actions: impl IntoIterator<Item = crate::window_toolbar::ToolbarAction>,
    ) {
        let inspector = toolbar
            .actions
            .iter()
            .position(|action| action.id == WINDOW_INSPECTOR)
            .map(|at| toolbar.actions.remove(at));
        toolbar.actions.extend(actions);
        if let Some(inspector) = inspector {
            // The toggle stands in the row; the app's menu-only actions
            // follow it in the menu.
            let at = toolbar.row_actions();
            toolbar.actions.insert(at, inspector);
        }
    }

    /// Run what the person did in the window's bar on the shell's own
    /// slots. Returns whether it was one of them; the app handles the rest.
    pub fn window_toolbar_event(&mut self, event: &ToolbarEvent) -> bool {
        match event {
            ToolbarEvent::ToggleSidebar => self.toggle_sidebar(),
            ToolbarEvent::Back => {
                self.back();
            }
            ToolbarEvent::Action { id } if id == WINDOW_BACK => {
                self.back();
            }
            ToolbarEvent::Action { id } if id == WINDOW_INSPECTOR => self.toggle_inspector(),
            ToolbarEvent::Action { id } if id == super::notices::WINDOW_NOTICE_ACTION => {
                self.press_notice_action();
            }
            _ => return false,
        }
        true
    }
}

/// The window bar's slots as the shell declares them for an app that
/// declares none: while the desktop's bar hosts the window, the headers'
/// titles and buttons are the bar's location and actions, and the shell
/// draws no header row. A header holding anything but buttons keeps the
/// row, as does a window where another part declares the slots. One
/// shell of a window declares them: the first that does, until it is gone.
pub(super) struct ShellBar {
    id: u64,
    /// The desktop's bar hosts the window ([`crate::window_toolbar::is_hosted`]).
    hosted: bool,
    /// Another part of the window declares the slots: the shell keeps its
    /// header row and declares nothing from then on.
    yielded: bool,
}

/// The shell that declares a window's slots, and what it declared.
type Owner = (u64, Option<WindowToolbar>);

thread_local! {
    /// Each window's declaring shell, by the window
    /// ([`crate::window_toolbar::current_window`]): a host that runs several
    /// apps on one thread (the browser desktop) shows each in a window of
    /// its own, and one window's shell must not take another's slots.
    static OWNERS: std::cell::RefCell<std::collections::HashMap<Option<usize>, Owner>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Read the current window's declaring shell.
fn with_owner<R>(read: impl FnOnce(Option<&Owner>) -> R) -> R {
    let window = crate::window_toolbar::current_window();
    OWNERS.with(|owners| read(owners.borrow().get(&window)))
}

/// Set (or with `None`, clear) the current window's declaring shell.
fn set_owner(owner: Option<Owner>) {
    let window = crate::window_toolbar::current_window();
    OWNERS.with(|owners| {
        let mut owners = owners.borrow_mut();
        match owner {
            Some(owner) => {
                owners.insert(window, owner);
            }
            None => {
                owners.remove(&window);
            }
        }
    });
}

impl Default for ShellBar {
    fn default() -> Self {
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1));
            id
        });
        Self {
            id,
            hosted: false,
            yielded: false,
        }
    }
}

impl ShellBar {
    /// Whether this shell may declare the window's slots: no other shell
    /// of the window does.
    fn may_declare(&self) -> bool {
        with_owner(|owner| owner.is_none_or(|(id, _)| *id == self.id))
    }

    /// Whether what the window's bar holds is another part's declaration
    /// than a shell's.
    fn declared_by_another_part() -> bool {
        let Some(declared) = crate::window_toolbar::declared() else {
            return false;
        };
        with_owner(|owner| owner.is_none_or(|(_, ours)| ours.as_ref() != Some(&declared)))
    }

    fn declare(&self, toolbar: Option<WindowToolbar>) {
        let owned = toolbar.is_some();
        crate::window_toolbar::declare(toolbar);
        set_owner(owned.then(|| (self.id, crate::window_toolbar::declared())));
    }

    fn owns(&self) -> bool {
        with_owner(|owner| owner.is_some_and(|(id, _)| *id == self.id))
    }
}

impl Drop for ShellBar {
    fn drop(&mut self) {
        // The slots go with the shell that declared them; a shell built in
        // its place declares its own. A shell dropped while another window
        // is current only gives up its claim: its own window goes with it.
        // A shell dropped as its thread ends finds the slots gone already.
        let window = crate::window_toolbar::current_window();
        let retract = OWNERS
            .try_with(|owners| {
                let mut owners = owners.borrow_mut();
                let mine: Vec<Option<usize>> = owners
                    .iter()
                    .filter(|(_, (id, _))| *id == self.id)
                    .map(|(window, _)| *window)
                    .collect();
                for owned in &mine {
                    owners.remove(owned);
                }
                mine.contains(&window)
            })
            .unwrap_or(false);
        if retract {
            crate::window_toolbar::retract();
        }
    }
}

impl NavShell {
    /// Whether the shell declares the slots from its headers now: not for
    /// an app that runs the sidebar toggle in slots of its own
    /// ([`Self::with_window_toolbar`], [`Self::set_sidebar_toggle_in_window_bar`]).
    pub(super) fn shell_bar_wanted(&self) -> bool {
        self.shell_bar.hosted
            && !self.shell_bar.yielded
            && self.shell_bar.may_declare()
            && !self.inputs.borrow().toggle_in_bar
            && self.header_slots().is_some()
    }

    /// Each column's header actions as the next build shows them.
    fn header_actions(&self, column: Column) -> &[super::Element] {
        match at(&self.pending.actions, column) {
            Some(actions) => actions,
            None => &at(&self.headers, column).actions,
        }
    }

    /// The headers' buttons as the bar's actions, each with the column and
    /// the path to it there (the action's place in the header, then the
    /// children down to the button), in column order. A row or group of
    /// buttons gives its buttons. `None` while a header holds something the
    /// bar cannot show: a leading fill, or a control that is not a button.
    fn header_slots(&self) -> Option<Vec<(ToolbarAction, Column, Vec<usize>)>> {
        let mut slots = Vec::new();
        for column in Column::ALL {
            // As in the shell's own header row, a column the shell does not
            // have offers nothing: its buttons are not the window's.
            if !self.has_column(column) {
                continue;
            }
            let leading = match at(&self.pending.leading, column) {
                Some(Staged(next)) => next.is_some(),
                None => at(&self.headers, column).leading.is_some(),
            };
            if leading {
                return None;
            }
            for (index, element) in self.header_actions(column).iter().enumerate() {
                let mut path = vec![index];
                collect_slots(element.as_ref(), column, &mut path, &mut slots)?;
            }
        }
        Some(slots)
    }

    /// Whether the shell has `column` as of its next build (shown or not:
    /// a closed sidebar is still the window's).
    fn has_column(&self, column: Column) -> bool {
        let inputs = self.inputs.borrow();
        match column {
            Column::Sidebar => inputs.has_sidebar,
            Column::Overview => inputs.has_overview,
            Column::Content => true,
            Column::Inspector => inputs.has_inspector,
        }
    }

    /// The header buttons the shell hands to the window's bar: drawn and
    /// pressed there, not laid out in the shell, yet the window's controls
    /// all the same. They publish their actions and take the operations a
    /// press in the bar starts, as they do in a header row.
    pub(super) fn bar_parts(&self) -> Vec<Part> {
        if !self.inputs.borrow().shell_bar {
            return Vec::new();
        }
        let exposed = self.exposed_parts();
        Column::ALL
            .into_iter()
            .filter(|column| self.has_column(*column))
            .flat_map(|column| {
                (0..at(&self.headers, column).actions.len()).map(move |i| Part::Action(column, i))
            })
            .filter(|part| !exposed.contains(part))
            .collect()
    }

    /// The header buttons among `parts` ([`Self::bar_parts`]), each once.
    pub(super) fn parts_mut(&mut self, parts: &[Part]) -> Vec<&mut (dyn Widget + 'static)> {
        let mut widgets = Vec::new();
        for (index, header) in self.headers.iter_mut().enumerate() {
            let Some(column) = Column::ALL.into_iter().find(|c| c.index() == index) else {
                continue;
            };
            for (i, action) in header.actions.iter_mut().enumerate() {
                if parts.contains(&Part::Action(column, i)) {
                    widgets.push(action.as_mut());
                }
            }
        }
        widgets
    }

    /// What the window shows, for the bar's location: the title of the
    /// column in front. Sidebar and inspector titles label their columns.
    fn shell_location(&self) -> Option<ToolbarLocation> {
        let title = |column: Column| at(&self.headers, column).title.title.clone();
        let front = if self.geometry.stack.len() > 1 {
            self.front_pane()
        } else {
            NavPane::Content
        };
        let label = match front {
            NavPane::Sidebar => None,
            NavPane::Overview => title(Column::Overview),
            NavPane::Content => title(Column::Content).or_else(|| title(Column::Overview)),
        }?;
        Some(ToolbarLocation {
            label,
            ..ToolbarLocation::default()
        })
    }

    /// Declare the slots from the headers while the shell owns them, follow
    /// the desktop's bar coming and going, and step aside for another part
    /// of the window that declares them (the sync pass).
    pub(super) fn sync_shell_bar(&mut self) {
        let hosted = crate::window_toolbar::is_hosted();
        if hosted != self.shell_bar.hosted {
            self.shell_bar.hosted = hosted;
            self.slots_changed();
        }
        if self.shell_bar.yielded {
            return;
        }
        if ShellBar::declared_by_another_part() {
            self.shell_bar.yielded = true;
            if self.shell_bar.owns() {
                set_owner(None);
            }
            self.slots_changed();
            return;
        }
        let slots = if self.inputs.borrow().shell_bar {
            self.header_slots()
        } else {
            None
        };
        let Some(slots) = slots else {
            if self.shell_bar.owns() {
                self.shell_bar.declare(None);
            }
            return;
        };
        let mut toolbar = self.window_toolbar();
        toolbar.location = self.shell_location();
        Self::push_window_actions(&mut toolbar, slots.into_iter().map(|(action, ..)| action));
        self.shell_bar.declare(Some(toolbar));
    }

    /// Run what the person did in the bar on the slots the shell declared:
    /// its own buttons, and a header button pressed as the keyboard
    /// presses it (the settle pass).
    pub(super) fn run_shell_bar_events(
        &mut self,
        edits: &mut crate::widget::Edits<'_>,
        path_root: &automerge::ObjId,
    ) {
        if !self.inputs.borrow().shell_bar {
            return;
        }
        let events = crate::window_toolbar::take_events();
        if events.is_empty() {
            return;
        }
        let slots = self.header_slots().unwrap_or_default();
        for event in events {
            if self.window_toolbar_event(&event) {
                continue;
            }
            let ToolbarEvent::Action { id } = event else {
                continue;
            };
            let Some((_, column, path)) = slots.iter().find(|(action, ..)| action.id == id) else {
                continue;
            };
            let Some((&first, rest)) = path.split_first() else {
                continue;
            };
            let header = &mut self.headers[column.index()];
            let element = match self.pending.actions[column.index()].as_mut() {
                Some(pending) => pending.get_mut(first),
                None => header.actions.get_mut(first),
            };
            let mut widget: Option<&mut dyn Widget> = element.map(|element| element.as_mut() as _);
            for &child in rest {
                widget = widget.and_then(|widget| widget.children_mut().into_iter().nth(child));
            }
            if let Some(widget) = widget {
                widget.perform_action(&crate::a11y::Action::Activate, &[], edits, path_root);
            }
        }
        self.invalidate();
    }
}

/// The buttons under `widget` as the bar's actions, at `path` in `column`'s
/// header. The app's search button is the bar's own search and gives none.
/// `None` for a control that is not a button: the header keeps its row.
fn collect_slots(
    widget: &dyn Widget,
    column: Column,
    path: &mut Vec<usize>,
    slots: &mut Vec<(ToolbarAction, Column, Vec<usize>)>,
) -> Option<()> {
    if is_app_search(widget) {
        return Some(());
    }
    let id = widget.test_id().filter(|id| !id.is_empty()).map_or_else(
        || {
            let at: Vec<String> = path.iter().map(usize::to_string).collect();
            format!("nav-shell.{}.{}", column.key(), at.join("."))
        },
        str::to_owned,
    );
    if let Some(action) = widget.window_slot(id) {
        slots.push((action, column, path.clone()));
        return Some(());
    }
    let children = widget.children();
    if children.is_empty() {
        // An empty row holds nothing to show; anything else on its own (a
        // label, a field, a picker) is not a button.
        return widget.accessibility().is_none().then_some(());
    }
    for (index, child) in children.into_iter().enumerate() {
        path.push(index);
        let collected = collect_slots(child, column, path, slots);
        path.pop();
        collected?;
    }
    Some(())
}
