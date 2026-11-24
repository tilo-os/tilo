//! A context menu over any element: right-click it, or press the Menu key
//! or Shift+F10 while focus is inside it, and the shared [`ContextMenu`]
//! opens with the commands its owner offers there and then.
//!
//! The entries are built when the menu opens, so they reflect what is
//! true at that moment (what is selected, which layer the row is). Each is a typed action: the
//! action the owner bound to the row ([`MenuEntry::bound`]), which the window runs on the app's
//! record, or an action the widget under the pointer declares as its own
//! ([`Widget::own_actions`]). A person clicks the row, an assistive technology or an agent
//! activates its node ([`ContextMenuArea::perform_semantic_action`]), and both run that one
//! action: never a press where the row is drawn.

use std::rc::Rc;

use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::{ContextMenu, Element, EventCtx, MenuItem, Widget};

use crate::theme::Theme;
use crate::widget::DrawChild;

/// One row of a context menu: what it shows, and what choosing it does.
pub struct MenuEntry {
    item: MenuItem,
    run: Option<Rc<dyn Fn()>>,
    /// What choosing it presses, when the view bound it: the action runs through the area's
    /// ownership, so a press of the row and anyone else running the action take the one path.
    press: crate::widget::Press,
    /// An emoji bar's way to the emoji picker: set when its last cell was chosen, and where
    /// the picked emoji goes.
    more: Option<(Rc<std::cell::Cell<bool>>, crate::component::Event)>,
}

impl MenuEntry {
    /// A command that presses what the view bound to it (`press`): the row runs the action
    /// itself, and the menu names it as the row's binding. Every command row is one of these —
    /// an app's action — or a widget's own action ([`Self::own`]): a row an agent is offered
    /// always has the action behind it.
    #[must_use]
    pub fn bound(label: impl Into<String>, press: crate::widget::Press) -> Self {
        Self {
            item: MenuItem::selection(label),
            run: None,
            press: crate::widget::Press::default(),
            more: None,
        }
        .for_press(press)
    }

    /// A row that runs an action the widget under the menu offers as its own
    /// ([`crate::widget::OwnAction`]): the area runs it where the widget stands, so the row
    /// holds no command of its own.
    #[must_use]
    fn own(label: impl Into<String>) -> Self {
        Self {
            item: MenuItem::selection(label),
            run: None,
            press: crate::widget::Press::default(),
            more: None,
        }
    }

    /// A line of secondary text about the entries around it (why the one
    /// above is unavailable): read, never chosen.
    pub fn caption(text: impl Into<String>) -> Self {
        Self {
            item: MenuItem::Caption(text.into()),
            run: None,
            press: crate::widget::Press::default(),
            more: None,
        }
    }

    /// A rule between groups of commands.
    pub fn separator() -> Self {
        Self {
            item: MenuItem::separator(),
            run: None,
            press: crate::widget::Press::default(),
            more: None,
        }
    }

    /// Choosing the entry presses what the view bound to it (`press`): the row runs the action
    /// itself, and the menu names it as the row's binding.
    #[must_use]
    pub fn for_press(mut self, press: crate::widget::Press) -> Self {
        if let Some(address) = press.address() {
            self.item = self.item.for_action(address);
        }
        self.press = press;
        self
    }

    /// Name the run of commands this one stands in (`group: "Due"`): consecutive
    /// entries of one run fold into a row that opens them
    /// ([`Self::fold_groups`]).
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.item = self.item.with_group(group);
        self
    }

    /// Say what choosing the row does beyond its name; assistive technology reads it.
    #[must_use]
    pub fn with_description(mut self, text: impl Into<String>) -> Self {
        self.item = self.item.with_description(text);
        self
    }

    /// The run of commands the entry stands in, when it names one.
    fn group(&self) -> Option<&str> {
        self.item.group()
    }

    /// A row that opens `entries` beside it: choosing one runs its command.
    #[must_use]
    pub fn submenu(
        label: impl Into<String>,
        icon: Option<crate::icons::Icon>,
        entries: Vec<MenuEntry>,
    ) -> Self {
        let enabled = entries.iter().any(|entry| entry.item.is_enabled());
        let checked = entries.iter().find_map(|entry| entry.item.checked());
        let members: Vec<MenuItem> = entries
            .into_iter()
            .map(|entry| {
                let mut item = match entry.item {
                    MenuItem::Action {
                        label,
                        icon,
                        checked,
                        destructive,
                        enabled,
                        shortcut,
                        description,
                        ..
                    } => {
                        let item = if destructive {
                            MenuItem::destructive_selection(label)
                        } else {
                            MenuItem::selection(label)
                        };
                        let item = match icon {
                            Some(icon) => item.with_icon(icon),
                            None => item,
                        };
                        let item = match checked {
                            Some(checked) => item.with_checked(checked),
                            None => item,
                        };
                        let item = item.with_enabled(enabled);
                        let item = match shortcut {
                            Some(shortcut) => item.with_shortcut(shortcut),
                            None => item,
                        };
                        match description {
                            Some(description) => item.with_description(description),
                            None => item,
                        }
                    }
                    other => other,
                };
                if let (Some(run), MenuItem::Action { action, .. }) = (entry.run.clone(), &mut item)
                {
                    *action = run;
                }
                if entry.press.is_none() {
                    item
                } else {
                    item.for_press(entry.press)
                }
            })
            .collect();
        let mut row = MenuItem::submenu(label, members);
        if let MenuItem::Action {
            icon: row_icon,
            enabled: row_enabled,
            checked: row_checked,
            ..
        } = &mut row
        {
            *row_icon = icon;
            *row_enabled = enabled;
            *row_checked = checked;
        }
        Self {
            item: row,
            run: None,
            press: crate::widget::Press::default(),
            more: None,
        }
    }

    /// The entries with each run of consecutive commands of one group (`group: "Due"`)
    /// folded into one entry named for the group, which opens them: the first entry
    /// keeps the run's icon and shows a check when one of its commands is on. A run
    /// of one command stays the command itself.
    #[must_use]
    pub fn fold_groups(entries: Vec<MenuEntry>) -> Vec<MenuEntry> {
        Self::fold_numbered(entries.into_iter().enumerate().collect()).0
    }

    /// [`Self::fold_groups`], keeping what each folded entry stands for: the command
    /// whose index it was given, or the indices of the run a group entry opens. A group
    /// entry runs nothing itself; its members carry their own commands.
    #[must_use]
    pub fn fold_numbered(
        rows: Vec<(usize, MenuEntry)>,
    ) -> (Vec<MenuEntry>, Vec<crate::widget::MenuPick>) {
        use crate::widget::MenuPick;
        let mut folded: Vec<MenuEntry> = Vec::with_capacity(rows.len());
        let mut picks: Vec<MenuPick> = Vec::with_capacity(rows.len());
        let mut rest = rows.into_iter().peekable();
        while let Some((index, entry)) = rest.next() {
            let Some(group) = entry.group().map(str::to_owned) else {
                folded.push(entry);
                picks.push(MenuPick::One(index));
                continue;
            };
            let mut run = vec![(index, entry)];
            while rest
                .peek()
                .is_some_and(|(_, next)| next.group() == Some(group.as_str()))
            {
                if let Some(next) = rest.next() {
                    run.push(next);
                }
            }
            if run.len() == 1 {
                let (index, entry) = run.pop().unwrap_or((index, MenuEntry::separator()));
                folded.push(entry);
                picks.push(MenuPick::One(index));
                continue;
            }
            let icon = match run[0].1.item() {
                MenuItem::Action { icon, .. } => *icon,
                _ => None,
            };
            let indices: Vec<usize> = run.iter().map(|(index, _)| *index).collect();
            let entries: Vec<MenuEntry> = run.into_iter().map(|(_, entry)| entry).collect();
            folded.push(MenuEntry::submenu(group, icon, entries));
            picks.push(MenuPick::Group(indices));
        }
        (folded, picks)
    }

    /// A row of emoji to pick one from, as a menu's emoji bar: picking one fires the bar's
    /// `pick` with it; its last cell, when the bar has `more`, opens the emoji picker over the
    /// menu's place for any other.
    pub fn emoji_bar(bar: super::attached::EmojiBarProps) -> Self {
        let super::attached::EmojiBarProps { emoji, more, pick } = bar;
        // The menu notes the cell chosen; the entry runs once the menu let go of the press.
        let chosen: Rc<std::cell::RefCell<Option<Chosen>>> = Rc::default();
        let note = Rc::clone(&chosen);
        let wants_more = Rc::clone(&chosen);
        let more = more.then(|| {
            Rc::new(move || {
                *wants_more.borrow_mut() = Some(Chosen::More);
            }) as super::context_menu::MenuAction
        });
        let item = MenuItem::emoji_row(
            emoji,
            move |emoji| *note.borrow_mut() = Some(Chosen::Emoji(emoji.to_owned())),
            more,
        );
        let picker = Rc::new(std::cell::Cell::new(false));
        let opens = Rc::clone(&picker);
        let fires = pick.clone();
        let run = Rc::new(move || match chosen.borrow_mut().take() {
            Some(Chosen::Emoji(emoji)) => fires.fire(Some(crate::component::Value::Text(emoji))),
            Some(Chosen::More) => opens.set(true),
            None => {}
        }) as Rc<dyn Fn()>;
        Self {
            item,
            run: Some(run),
            press: crate::widget::Press::default(),
            more: Some((picker, pick)),
        }
    }

    /// The keys that do the same, shown at the row's end.
    #[must_use]
    pub fn with_shortcut(mut self, keys: impl Into<crate::platform::Shortcut>) -> Self {
        self.item = self.item.with_shortcut(keys);
        self
    }

    /// It takes something away: it is undone, not confirmed, and is drawn as such.
    #[must_use]
    pub fn with_destructive(mut self, destructive: bool) -> Self {
        self.item = self.item.with_destructive(destructive);
        self
    }

    /// The choice this row is now, for the ones that are a state.
    #[must_use]
    pub fn with_checked(mut self, checked: bool) -> Self {
        self.item = self.item.with_checked(checked);
        self
    }

    /// Shown but not available now.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.item = self.item.with_enabled(enabled);
        self
    }

    /// A leading icon for recognition; the label stays its name.
    #[must_use]
    pub fn with_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.item = self.item.with_icon(icon);
        self
    }

    /// The row as the menu shows it.
    pub fn item(&self) -> &MenuItem {
        &self.item
    }

    /// What choosing the row runs, for a test that chooses it on a menu of its own.
    #[cfg(test)]
    pub(crate) fn run_for_tests(&self) -> Option<Rc<dyn Fn()>> {
        self.run.clone()
    }
}

/// What an emoji row was chosen for.
enum Chosen {
    Emoji(String),
    More,
}

/// An action of the widget under the pointer that a menu row runs where it stands: the child
/// indices that lead to the widget, and the action [`Widget::perform_action`] runs there.
#[derive(Clone)]
struct OwnChoice {
    path: Vec<usize>,
    action: crate::widget::OwnAction,
}

/// The deepest widget of `widget`'s subtree drawn under `at` that offers actions of its own —
/// a link inside the row the menu opened over answers before the row does — with the child
/// indices that lead to it.
fn own_actions_under(
    widget: &dyn Widget,
    at: Point,
) -> Option<(Vec<usize>, Vec<crate::widget::OwnAction>)> {
    let children = widget.children();
    for (index, child) in children.iter().enumerate().rev() {
        if !child.get_rect().contains(at) {
            continue;
        }
        if let Some((mut path, actions)) = own_actions_under(*child, at) {
            path.insert(0, index);
            return Some((path, actions));
        }
    }
    let own = widget.own_actions();
    (!own.is_empty()).then_some((Vec::new(), own))
}

/// The widget `path` leads to under `root`, for a row that runs what it offers as its own.
fn widget_at<'a>(widget: &'a mut dyn Widget, path: &[usize]) -> Option<&'a mut dyn Widget> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(widget);
    };
    let child = widget.children_mut().into_iter().nth(index)?;
    widget_at(child, rest)
}

/// The entries a menu offers where it opens, at the pointer in window
/// coordinates (`None` when the keyboard opened it).
pub type MenuEntries = Box<dyn Fn(Option<Point>) -> Vec<MenuEntry>>;

/// The entries a menu offers where it opens, asked of the content: a list
/// answers for the row under the pointer, or its focused row when the
/// keyboard opened the menu (`None`).
pub type ContentMenuEntries = Box<dyn Fn(&dyn Widget, Option<Point>) -> Vec<MenuEntry>>;

/// What opened the open menu: where it must stay.
#[derive(Clone, Copy)]
enum Opening {
    /// The pointer, at that point.
    Pointer(Point),
    /// The keyboard, on the item that has it: read again every frame, so the
    /// menu follows the row through a scroll, a resize or a text scale.
    Item,
}

/// The children an area stands over its content, in the order a route reaches them: the open
/// menu, then the emoji picker an emoji bar opened over it. Written once — the read-only list,
/// the walk and the list handed out to change all expand it — so a child a route reaches by its
/// index is the child the walk found, however the list is asked for.
macro_rules! over_content {
    ($area:ident $(, $mut:tt)?) => {{
        let mut over: Vec<&$($mut)? (dyn Widget + '_)> = Vec::new();
        if $area.menu.is_open() {
            over.push(&$($mut)? $area.menu);
        }
        if let Some((picker, _)) = &$($mut)? $area.picker {
            over.push(picker);
        }
        over
    }};
}

#[crate::component::not_a_component(part, "what the `ui.ContextMenu` attachment wraps its view in")]
pub struct ContextMenuArea {
    content: Element,
    entries: ContentMenuEntries,
    /// Whether what is offered where a menu opens is on its way ([`Self::waiting_on`]).
    waits: Option<Box<dyn Fn(&dyn Widget, Option<Point>) -> bool>>,
    /// A menu asked for that waits for what it offers: it opens where it was asked once that
    /// comes.
    pending: Option<(Rect, Option<Point>)>,
    menu: ContextMenu,
    /// What opened the open menu, and so where it stands.
    opening: Option<Opening>,
    runs: Vec<Option<Rc<dyn Fn()>>>,
    /// What each entry presses, in step with `runs`.
    presses: Vec<crate::widget::Press>,
    /// The actions the widget under the pointer offers as its own, in step with `runs`: an entry
    /// that is one of them runs on the widget it was opened over.
    own: Vec<Option<OwnChoice>>,
    cursor: Point,
    /// The pointer's last move, replayed to the content once a command
    /// ran, so what the command asked of it (a rename beginning) happens
    /// at once rather than at the pointer's next move.
    moved: Option<WindowEvent>,
    viewport: Option<Rect>,
    /// The open menu's emoji bars' ways to the emoji picker.
    more: Vec<(Rc<std::cell::Cell<bool>>, crate::component::Event)>,
    /// The emoji picker an emoji bar opened, and where its pick goes.
    picker: Option<(super::EmojiPicker, crate::component::Event)>,
    /// The picker to open over the menu's place at the next drawing, which knows the theme.
    picker_wanted: Option<(Rect, crate::component::Event)>,
    /// The host's input capabilities, which the menu hears whenever they
    /// change: its rows are the size of a target even before it opens.
    environment: crate::interaction::InteractionEnvironment,
}

impl ContextMenuArea {
    /// `content` with a menu of what `entries` offers when it opens.
    pub fn new(
        content: Element,
        entries: impl Fn(Option<Point>) -> Vec<MenuEntry> + 'static,
    ) -> Self {
        Self::over_content(content, move |_, at| entries(at))
    }

    /// `content` with a menu of what `entries` offers when it opens, asked
    /// with the content itself, so one menu can serve every row of a list.
    pub fn over_content(
        content: Element,
        entries: impl Fn(&dyn Widget, Option<Point>) -> Vec<MenuEntry> + 'static,
    ) -> Self {
        Self {
            content,
            entries: Box::new(entries),
            waits: None,
            pending: None,
            menu: ContextMenu::new(),
            opening: None,
            runs: Vec::new(),
            presses: Vec::new(),
            own: Vec::new(),
            cursor: Point::ZERO,
            moved: None,
            viewport: None,
            more: Vec::new(),
            picker: None,
            picker_wanted: None,
            environment: crate::interaction::InteractionEnvironment::default(),
        }
    }

    /// The area with `waits` saying whether what is offered at a point (or under the keyboard)
    /// is on its way: a menu opened there waits for it, and opens once it has come.
    #[must_use]
    pub fn waiting_on(
        mut self,
        waits: impl Fn(&dyn Widget, Option<Point>) -> bool + 'static,
    ) -> Self {
        self.waits = Some(Box::new(waits));
        self
    }

    /// What the menu is over.
    pub fn content(&self) -> &dyn Widget {
        self.content.as_ref()
    }

    /// What the menu is over, to update in place.
    pub fn content_mut(&mut self) -> &mut dyn Widget {
        self.content.as_mut()
    }

    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// The menu, for inspection.
    pub fn menu(&self) -> &ContextMenu {
        &self.menu
    }

    /// Open the menu at `at` (window coordinates) with what is offered
    /// there now; whether there was anything to offer. A keyboard opening
    /// names the item it stands under instead ([`Self::open_over`]).
    pub fn open_at(&mut self, at: Point, pointer: bool) -> bool {
        let anchor = Rect::new(at.x, at.y, at.x, at.y);
        self.open_over(anchor, pointer.then_some(at))
    }

    /// Open the menu over `anchor` — a row, or a point — with what is
    /// offered there now; whether there was anything to offer.
    ///
    /// `at_pointer` is where the pointer is, when the pointer opened it: the
    /// entries answer for it, and a question typed into the menu is about
    /// it. The keyboard names the item instead, and the menu stands under
    /// that.
    pub fn open_over(&mut self, anchor: Rect, at_pointer: Option<Point>) -> bool {
        self.pending = None;
        let mut entries = (self.entries)(self.content.as_ref(), at_pointer);
        // A row of commands is something to offer whether or not the view bound it (a
        // described row the host left unbound still shows, so a missing binding is seen
        // rather than silently emptying the menu); captions and rules are not.
        let owner_offers = entries
            .iter()
            .any(|entry| !matches!(entry.item, MenuItem::Separator | MenuItem::Caption(_)));
        // What the menu adds itself — the row that asks what is typed, and the send row after
        // it — is nobody's answer to what was asked for at this place: with nothing of the
        // owner's to offer, a menu that is waiting for its entries stands aside rather than
        // opening on those rows alone and never taking the ones that come.
        let waiting = !owner_offers
            && self
                .waits
                .as_ref()
                .is_some_and(|waits| waits(self.content.as_ref(), at_pointer));
        if waiting {
            self.opening = None;
            self.pending = Some((anchor, at_pointer));
            return false;
        }
        // The actions the widget under the pointer offers as its own stand beside its owner's
        // rows: a link's copy belongs to the link, whether the row around it offers nothing or
        // a menu of its own.
        let mut own: Vec<Option<OwnChoice>> = vec![None; entries.len()];
        if let Some((path, actions)) =
            at_pointer.and_then(|at| own_actions_under(self.content.as_ref(), at))
        {
            for action in actions {
                entries.push(MenuEntry::own(action.name.clone()));
                own.push(Some(OwnChoice {
                    path: path.clone(),
                    action,
                }));
            }
        }
        let offered = entries
            .iter()
            .any(|entry| !matches!(entry.item, MenuItem::Separator | MenuItem::Caption(_)));
        let mut menu = ContextMenu::new();
        self.runs.clear();
        self.presses.clear();
        self.more.clear();
        self.own.clear();
        if offered {
            for entry in entries {
                menu.push(entry.item);
                self.runs.push(entry.run);
                self.presses.push(entry.press);
                self.more.extend(entry.more);
            }
            self.own = own;
            // The entries' commands are the area's: it keeps them beside the menu so a choice
            // runs whether the menu still stands, and a row a tool names is reported to it.
            menu.rows_are_the_owners();
        }
        // A right-click inside what is selected in the content is about the
        // selection, so a question typed into the menu is; elsewhere the app
        // names what is under the pointer, else the window
        // (`shared_selection::pointed_selection`, the one subject rule).
        menu.set_about(crate::shared_selection::pointed_selection(
            crate::shared_selection::subtree_selection(self.content.as_ref()),
            at_pointer,
        ));
        menu.set_interaction_environment(self.environment);
        if let Some(viewport) = self.viewport {
            menu.set_viewport(Some(viewport));
        }
        menu.open_over(anchor);
        self.opening = Some(match at_pointer {
            Some(at) => Opening::Pointer(at),
            None => Opening::Item,
        });
        // Nothing offered and nothing of the menu's own to show (no row that asks what is
        // typed): no menu.
        if menu.items().is_empty() {
            self.opening = None;
            return false;
        }
        menu.focus_first_action();
        self.menu = menu;
        true
    }

    /// Stand the open menu where what opened it is now: the item with the
    /// keyboard may have scrolled, the window may have resized, and the menu
    /// stays under it.
    fn place_menu(&mut self) {
        let Some(opening) = self.opening.filter(|_| self.menu.is_open()) else {
            return;
        };
        match opening {
            Opening::Pointer(at) => {
                if let Some(viewport) = self.viewport {
                    self.menu
                        .reanchor(Rect::new(at.x, at.y, at.x, at.y), viewport);
                }
            }
            Opening::Item => {
                let Some(viewport) = self.viewport else {
                    return;
                };
                // The row it stands under is gone from the frame it still
                // paints over (scrolled out): it stays where it is rather
                // than jumping to the foot of the list.
                let Some(anchor) = keyboard_anchor(self.content.as_ref()) else {
                    return;
                };
                self.menu.reanchor(anchor, viewport);
            }
        }
    }

    /// Choose the entry at `index` as a click would.
    pub fn choose(&mut self, index: usize) {
        // A widget's own action changes the widget, so it runs where the event that chose it
        // carries the document ([`Self::run`]); a choice that has none runs nothing.
        if self.own.get(index).is_some_and(Option::is_some) {
            self.menu.close();
            return;
        }
        if let Some(press) = self.presses.get(index).filter(|press| !press.is_none()) {
            self.menu.close();
            press.run();
            return;
        }
        if let Some(Some(run)) = self.runs.get(index) {
            self.menu.close();
            let run = Rc::clone(run);
            run();
            return;
        }
        // A row the menu adds itself (asking what is typed) runs its own.
        self.menu.click_item(index);
        self.menu.close();
    }

    /// Run the entry at `index` on the action behind it, and close the menu: the owner's action
    /// the row is bound to, the widget's own action where the widget stands (which changes it,
    /// so it runs with the document the choosing event carries), or the row the menu adds
    /// itself. Whether one of those ran it.
    fn activate(
        &mut self,
        index: usize,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        if let Some(Some(choice)) = self.own.get(index).cloned() {
            self.menu.close();
            if let Some(widget) = widget_at(self.content.as_mut(), &choice.path) {
                widget.perform_action(
                    &crate::a11y::Action::Activate,
                    &choice.action.key_path,
                    state,
                    path,
                );
            }
            return true;
        }
        if let Some(press) = self.presses.get(index).filter(|press| !press.is_none()) {
            self.menu.close();
            press.run();
            return true;
        }
        if let Some(Some(run)) = self.runs.get(index) {
            self.menu.close();
            let run = Rc::clone(run);
            run();
            return true;
        }
        false
    }

    /// Run the chosen entry, then let the content see the pointer where
    /// it is, so what the command asked of it happens now.
    fn run(&mut self, index: usize, ctx: &mut EventCtx<'_>) {
        let place = self.menu.get_rect();
        if !self.activate(index, &mut ctx.state.reborrow(), ctx.path) {
            // A row the menu adds itself (asking what is typed) runs its own.
            self.menu.click_item(index);
            self.menu.close();
        }
        // An emoji bar's last cell opens the picker where the menu was.
        if let Some((_, pick)) = self.more.iter().find(|(wanted, _)| wanted.replace(false)) {
            self.picker_wanted = Some((place, pick.clone()));
        }
        if let Some(moved) = self.moved.clone() {
            self.content.handle_event(&moved, ctx);
        }
    }

    /// Where a gesture opens the menu: the row, control or point it stands
    /// against, and the pointer's position when the pointer opened it.
    fn opens(&self, event: &WindowEvent, ctx: &EventCtx<'_>) -> Option<(Rect, Option<Point>)> {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.content.get_rect().contains(self.cursor) => {
                let at = self.cursor;
                Some((Rect::new(at.x, at.y, at.x, at.y), Some(at)))
            }
            _ if super::context_menu::opens_from_keyboard(event, ctx.modifiers.state())
                && focus_within(self.content.as_ref()) =>
            {
                // The item that has the keyboard, as every platform opens a
                // context menu from it; the focused widget itself where the
                // content holds no items of its own.
                let anchor = keyboard_anchor(self.content.as_ref())
                    .unwrap_or_else(|| self.content.get_rect());
                Some((anchor, None))
            }
            _ => None,
        }
    }
}

/// The box a menu opened from the keyboard stands under: the item with the
/// keyboard, deepest first, so a list answers for its focused row rather
/// than for itself.
pub(crate) fn keyboard_anchor(widget: &dyn Widget) -> Option<Rect> {
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = keyboard_anchor(child);
        }
    });
    found
        .or_else(|| widget.context_menu_anchor())
        .or_else(|| widget.is_focused().then(|| widget.get_rect()))
}

/// An operation goes to the content, as it would without the menu, then to
/// the open menu: a row an agent chooses runs the action it names.
impl super::ResourceOperations for ContextMenuArea {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        super::local_resource_operation_handlers(self.content.as_ref())
    }

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers = super::resource_operation_handlers(self.content.as_ref());
        if self.menu.is_open() {
            handlers.extend(super::resource_operation_handlers(&self.menu));
        }
        handlers
    }

    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        doc: &mut super::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        if let Some(started) = super::start_operation(self.content.as_mut(), op, doc, path)? {
            return Ok(Some(started));
        }
        if self.menu.is_open() {
            return super::start_operation(&mut self.menu, op, doc, path);
        }
        Ok(None)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        super::poll_resource_operation(
            self.content.as_mut(),
            invocation,
            cancellation_requested,
            doc,
            path,
        )
        .or_else(|| {
            super::poll_resource_operation(
                &mut self.menu,
                invocation,
                cancellation_requested,
                doc,
                path,
            )
        })
    }
}

/// Whether `widget` or anything inside it has the keyboard focus.
pub(super) fn focus_within(widget: &dyn Widget) -> bool {
    if widget.is_focused() {
        return true;
    }
    let mut found = false;
    widget.for_each_child(&mut |child| found |= focus_within(child));
    found
}

impl super::Decorator for ContextMenuArea {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    // The area answers operations and controls itself, so one addressed to a
    // row of its open menu reaches the menu even when the content answers its
    // own, and a row chosen from it hands over its action after the menu
    // closed. Every other capability is the content's.
    // What it wraps is answered as any decorator's is, so a walk that
    // reaches into what a view wrapped finds the view (see
    // [`crate::widget::find_mut`]).
    crate::provides!(
        dyn super::ResourceOperations,
        dyn crate::widget::controls::ActionControls,
        dyn crate::widget::decorator::Decorated,
        dyn crate::widget::decorator::DecoratedMut;
        inner = content
    );

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
            self.moved = Some(event.clone());
        }
        if let Some((picker, pick)) = self.picker.as_mut() {
            picker.handle_event(event, ctx);
            for happened in picker.take_events() {
                if let super::emoji_picker::PickerEvent::Picked(emoji) = happened {
                    pick.fire(Some(crate::component::Value::Text(emoji)));
                }
                picker.close();
            }
            if !picker.is_open() {
                self.picker = None;
            }
            *ctx.handled = true;
            return;
        }
        if self.menu.is_open() {
            // The menu itself tells the host's replay of the still opening
            // pointer from a real move, so the keyboard keeps its row.
            self.menu.handle_event(event, ctx);
            // The menu reports a row as chosen when the press that took it comes up (or the
            // key that chose it lands): it stays open, and its own press, until then.
            if let Some(index) = self.menu.take_selection() {
                self.menu.close();
                self.run(index, ctx);
                *ctx.handled = true;
            }
            if self.menu.is_open() || *ctx.handled {
                return;
            }
        }
        // A press elsewhere means the person moved on from a menu still on its way.
        let pressed = match event {
            WindowEvent::MouseInput { state, .. } => *state == ElementState::Pressed,
            WindowEvent::KeyboardInput { event, .. } => event.state == ElementState::Pressed,
            _ => false,
        };
        if pressed {
            self.pending = None;
        }
        if let Some((anchor, at_pointer)) = self.opens(event, ctx) {
            // A right-click reaches the content first, so it can take it
            // (a field's own menu) or act on where it landed (pick what is
            // under the pointer, as the menu is then about it).
            if at_pointer.is_some() {
                self.content.handle_event(event, ctx);
                if *ctx.handled {
                    return;
                }
            }
            if self.open_over(anchor, at_pointer) || self.pending.is_some() {
                *ctx.handled = true;
            } else if at_pointer.is_none() {
                self.content.handle_event(event, ctx);
            }
            return;
        }
        self.content.handle_event(event, ctx);
    }

    fn concrete_type_name(&self) -> &'static str {
        "ContextMenuArea"
    }

    /// The content's change is the area's: the menu adds no structure of
    /// its own, so rows a list mounts in place stay mounted in place.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.content.take_structure_change()
    }

    // The content's node is this area's: named targets and hit tests that
    // find the node meet the area, which answers as the content does.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.content.build_layout(taffy, theme, node_map);
        super::decorator::register_node(node_map, node, self);
        node
    }

    // The menu's box is not the content's: the pass places it too, so its
    // edges land on the display's pixels as the content's do.
    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.content.apply_layout(taffy, node, origin);
        // What a menu waited for has come with the content laid out anew.
        if let Some((anchor, at_pointer)) = self.pending {
            self.open_over(anchor, at_pointer);
        }
        self.menu.layout_at_position();
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_open() || self.picker.is_some() || self.content.wants_pointer_capture()
    }

    // The menu is a child only while it is open, so it hears the host's
    // input capabilities whenever they change, closed or not: its rows are
    // the size of a target the next time it opens.
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.content.set_interaction_environment(environment);
        self.menu.set_interaction_environment(environment);
        self.environment = environment;
    }

    /// What a frame paints: the content's own change, what the menu paints
    /// over it (which is nothing while it is closed, [`ContextMenu::needs_redraw`]),
    /// and a picker it stands open. A box without the menu is the
    /// menu's painting too.
    fn needs_redraw(&self) -> bool {
        self.content.needs_redraw()
            || self.menu.needs_redraw()
            || self.content.needs_animation_frame()
            || self.picker_wanted.is_some()
            || self
                .picker
                .as_ref()
                .is_some_and(|(picker, _)| picker.needs_redraw() || picker.needs_animation_frame())
    }

    fn needs_animation_frame(&self) -> bool {
        self.content.needs_animation_frame()
            || self.menu.needs_animation_frame()
            || self.picker_wanted.is_some()
            || self
                .picker
                .as_ref()
                .is_some_and(|(picker, _)| picker.needs_animation_frame())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu
            .modal_capture()
            .or_else(|| self.picker.as_ref().map(|(picker, _)| picker.rect()))
            .or_else(|| self.content.modal_capture())
    }

    fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        // The overlay pass sees the whole window, where the menu may go;
        // the content's own pass may be scoped to a scrolled list.
        if let Some(viewport) = env.viewport {
            self.viewport = Some(viewport);
            // The row it stands under may have scrolled, and the window may
            // have changed: the menu stands where what opened it is now.
            self.place_menu();
        }
        self.content.draw_overlay(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text_renderer,
            theme,
            env.clone(),
        ));
        // The menu paints in the overlay pass, over everything.
        self.menu.draw_overlay(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text_renderer,
            theme,
            env.clone(),
        ));
        if let Some((anchor, pick)) = self.picker_wanted.take() {
            let mut picker = super::EmojiPicker::framed();
            let viewport = self.viewport.unwrap_or_else(|| self.content.get_rect());
            picker.open_at(Some(anchor), viewport, theme);
            self.picker = Some((picker, pick));
        }
        if let Some((picker, _)) = self.picker.as_mut() {
            picker.draw_child(scene, transform, text_renderer, theme, &env);
        }
    }

    // An open menu is part of the tree, so assistive technology and
    // inspection see it, and so is the picker an emoji bar opened over it.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.content.children();
        children.extend(over_content!(self));
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.content.for_each_child(visitor);
        for child in over_content!(self) {
            visitor(child);
        }
    }

    /// This area stands for its content's node, laid out under it.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(self.content.as_ref());
        visitor(&self.menu);
        true
    }

    /// The menu and the box it carries, and an emoji picker, are placed by the area itself, over
    /// the content, where the window's layout gives them no node: an assistive technology and an
    /// agent reach their controls through the content's node, as a person reaches them by pointer.
    fn lays_out_child(&self, child_index: usize) -> bool {
        self.menu.is_open() && child_index == self.content.children().len()
            || self.picker.is_some() && child_index == self.content.children().len() + 1
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self.content.children_mut();
        children.extend(over_content!(self, mut));
        children
    }

    /// A row of the open menu is the owner's command: an assistive technology, a script or an
    /// agent that activates the row's node runs the action the row names — the app's, or the
    /// widget's own — and the menu closes on it. It is never a press where the row is drawn, so
    /// what an agent chooses is the action behind the row, decided by that action's own
    /// permissions and confirmation.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        if matches!(action, crate::a11y::Action::Activate)
            && self.menu.is_open()
            && let Some(index) = target
                .strip_prefix("context-menu:item:")
                .and_then(|index| index.parse::<usize>().ok())
            && self
                .menu
                .items()
                .get(index)
                .is_some_and(MenuItem::is_enabled)
        {
            return self.activate(index, state, path);
        }
        // Every other target is the content's, as it is without the menu.
        self.content
            .perform_semantic_action(target, action, state, path)
    }
}

impl crate::widget::controls::ActionControls for ContextMenuArea {
    fn attach_controls(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_domain::DomainAttachment,
    ) {
        crate::widget::controls::attach_controls(&mut *self.content, domain, attachment);
        for press in &self.presses {
            press.attach(domain, attachment);
        }
        crate::widget::controls::attach_controls(&mut self.menu, domain, attachment);
    }

    /// A row pressed before the menu closed hands its invocation over here.
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        crate::widget::controls::take_control_invocation(&mut *self.content)
            .or_else(|| self.presses.iter().find_map(crate::widget::Press::take))
            .or_else(|| crate::widget::controls::take_control_invocation(&mut self.menu))
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::control_action_bindings(&*self.content);
        bindings.extend(crate::widget::controls::control_action_bindings(&self.menu));
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        crate::widget::controls::refresh_control_action_availability(&mut *self.content, allowed);
        for press in &self.presses {
            press.refresh_availability(allowed);
        }
        crate::widget::controls::refresh_control_action_availability(&mut self.menu, allowed);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use winit::keyboard::{Key, NamedKey};

    use super::*;
    use crate::widget::Spacer;

    fn moved(at: Point) -> WindowEvent {
        crate::synthetic_input::cursor_moved(at.x, at.y)
    }

    fn right_release() -> WindowEvent {
        crate::synthetic_input::mouse_input(ElementState::Released, MouseButton::Right)
    }

    /// What the entry just chosen ran: the typed action it names, taken from the press the area
    /// keeps for it.
    fn chosen_action(area: &mut ContextMenuArea) -> Option<String> {
        crate::widget::controls::take_control_invocation(area).map(|ran| ran.address.action.clone())
    }

    fn opening_menu() -> (ContextMenuArea, Point) {
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), |_| {
            (1..=4)
                .map(|index| {
                    MenuEntry::bound(
                        format!("Action {index}"),
                        crate::testing::bound_press("test:menu", &format!("action-{index}")),
                    )
                })
                .collect()
        });
        let at = Point::new(20.0, 20.0);
        crate::testing::dispatch(&mut area, &moved(at));
        assert!(area.open_at(at, true));
        (area, at)
    }

    fn named_key(key: NamedKey) -> WindowEvent {
        crate::synthetic_input::key_input(&Key::Named(key), ElementState::Pressed)
    }

    #[test]
    fn movement_during_opening_keeps_the_pointer_row_for_enter() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut area, _) = opening_menu();
        let Some(row) = area.menu().item_rect(1) else {
            panic!("the second action must have a hit target");
        };
        crate::testing::dispatch(&mut area, &moved(row.center()));
        crate::testing::dispatch(&mut area, &right_release());
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &named_key(NamedKey::Enter));
        assert_eq!(chosen_action(&mut area).as_deref(), Some("action-2"));
    }

    #[test]
    fn keyboard_choice_before_opening_release_survives_stationary_replay() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut area, at) = opening_menu();
        crate::testing::dispatch(&mut area, &named_key(NamedKey::ArrowDown));
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &moved(at));
        crate::testing::dispatch(&mut area, &right_release());
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &named_key(NamedKey::Enter));
        assert_eq!(chosen_action(&mut area).as_deref(), Some("action-2"));
    }

    #[test]
    fn a_menu_over_its_content_is_asked_with_the_content() {
        let mut area = ContextMenuArea::over_content(
            Box::new(Spacer::fixed(10.0).with_id("rows")),
            |content, at| {
                let named = content.test_id().unwrap_or_default().to_owned();
                vec![MenuEntry::bound(
                    format!("{named} {}", at.is_some()),
                    crate::testing::bound_press("test:menu", "rows"),
                )]
            },
        );
        assert!(area.open_at(Point::new(5.0, 5.0), false));
        assert_eq!(
            area.menu().items().first().and_then(MenuItem::label),
            Some("rows false")
        );
        assert_eq!(area.content().test_id(), Some("rows"));
    }

    #[test]
    fn entries_are_built_when_it_opens_and_choosing_runs_one() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let offered = Rc::new(Cell::new(1));
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), {
            let offered = Rc::clone(&offered);
            move |_| {
                let n = offered.get();
                vec![
                    MenuEntry::bound(
                        format!("Take {n}"),
                        crate::testing::bound_press("test:menu", &format!("take-{n}")),
                    )
                    .with_shortcut("Ctrl+T"),
                    MenuEntry::separator(),
                ]
            }
        });
        offered.set(7);
        assert!(area.open_at(Point::new(5.0, 5.0), true));
        assert!(area.is_open());
        assert_eq!(
            area.menu().items().first().and_then(MenuItem::label),
            Some("Take 7")
        );
        assert_eq!(
            area.menu().items().first().and_then(MenuItem::shortcut),
            Some(&crate::platform::Shortcut::from("Ctrl+T"))
        );
        area.choose(0);
        assert_eq!(chosen_action(&mut area).as_deref(), Some("take-7"));
        assert!(!area.is_open());
    }

    /// The other kind of entry: a row that is the widget's own declared action. An agent
    /// activating it runs that action where the widget stands and the menu closes — the copy is
    /// the link's own, not a press on the row.
    #[test]
    fn an_agent_chooses_a_widgets_own_action_from_its_row() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        use crate::clipboard::{ClipboardBackend as _, ClipboardTarget, InProcessClipboard};
        let clipboard = std::sync::Arc::new(InProcessClipboard::default());
        let services = crate::process::ProcessServices::null().with_clipboard(clipboard.clone());
        crate::process::scoped(&services, || {
            let area = ContextMenuArea::new(
                Box::new(
                    crate::widget::Link::new("https://example.org/page").with_label("The page"),
                ),
                |_| {
                    vec![MenuEntry::bound(
                        "Open",
                        crate::testing::bound_press("test:menu", "open"),
                    )]
                },
            );
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                area,
                (400, 120),
                crate::theme::Theme::dark(),
            );
            harness.relayout();
            let page = harness
                .root::<ContextMenuArea>()
                .content()
                .get_rect()
                .center();
            harness.send(&crate::synthetic_input::cursor_moved(page.x, page.y));
            right_click(&mut harness);
            let target = crate::a11y_child_route::encode(
                &[harness.root::<ContextMenuArea>().content().children().len()],
                Some("context-menu:item:1"),
            );
            harness
                .perform_semantic_named("Link", Some(&target), &crate::a11y::Action::Activate)
                .expect("the link's copy row is reachable");
            assert_eq!(
                clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                Some("https://example.org/page"),
                "the row ran the link's own action where the link stands"
            );
            assert!(
                !harness.root::<ContextMenuArea>().is_open(),
                "the menu closed on its choice"
            );
        });
    }

    /// A row an assistive technology or an agent names is the entry the area runs through the
    /// action the row names: the menu leaves its owner's rows to the area, which runs the app's
    /// action once and closes the menu. No press lands where the row is drawn.
    #[test]
    fn a_row_a_tool_names_is_the_entry_the_area_runs() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut area, _) = opening_menu();
        let mut doc = automerge::AutoCommit::new();
        let target = crate::a11y_child_route::encode(&[0], Some("context-menu:item:1"));
        assert!(
            crate::a11y_child_route::perform(
                &mut area,
                &target,
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "the area runs the row its menu offers"
        );
        assert_eq!(
            chosen_action(&mut area).as_deref(),
            Some("action-2"),
            "the row ran the action it names"
        );
        assert!(
            chosen_action(&mut area).is_none(),
            "an activation runs the entry once"
        );
        assert!(!area.is_open(), "the menu closed on the row");
    }

    type Chosen = Rc<std::cell::RefCell<Vec<String>>>;

    /// The picker an emoji bar opened stands over the menu, so it is one of the children a
    /// route reaches by index — in the list the walk reads and in the one an action is
    /// dispatched through, at the same place.
    #[test]
    fn the_emoji_picker_over_the_menu_is_a_child_a_route_reaches() {
        let (mut area, _) = opening_menu();
        let mut picker = crate::widget::EmojiPicker::framed();
        picker.open_at(None, Rect::new(0.0, 0.0, 400.0, 400.0), &Theme::dark());
        let place = picker.rect();
        area.picker = Some((picker, crate::component::Event::None));
        let picker_at = area
            .picker
            .as_ref()
            .map(|(picker, _)| std::ptr::from_ref(picker).cast::<()>());

        let listed: Vec<*const ()> = area
            .children()
            .into_iter()
            .map(|child| std::ptr::from_ref(child).cast::<()>())
            .collect();
        let mut walked: Vec<*const ()> = Vec::new();
        area.for_each_child(&mut |child| walked.push(std::ptr::from_ref(child).cast::<()>()));
        let handed: Vec<*const ()> = area
            .children_mut()
            .into_iter()
            .map(|child| std::ptr::from_ref(&*child).cast::<()>())
            .collect();
        assert_eq!(
            listed, walked,
            "the walk visits the children the list names"
        );
        assert_eq!(
            listed, handed,
            "children_mut hands out the children the walk found, so an action a route addresses \
             reaches the child it was walked to"
        );
        assert_eq!(listed.last().copied(), picker_at, "the picker stands last");
        let index = listed.len() - 1;
        assert_eq!(
            crate::a11y_child_route::rect(&area, &crate::a11y_child_route::encode(&[index], None)),
            Some(place),
            "the index names the picker, where it stands over the menu"
        );
    }

    const MENU_LABELS: [&str; 4] = ["First", "Second", "Third", "Fourth"];

    /// A 300 px area over an app whose record offers the four rows, in a window whose host
    /// replays a still pointer after the widgets change, the pointer resting at (100, 100).
    fn four_entries() -> (crate::headless::FrameHarness, Chosen) {
        let chosen = Chosen::default();
        let content = crate::testing::MenuRows::new(&MENU_LABELS, Rc::clone(&chosen));
        let area = ContextMenuArea::new(Box::new(content), |_| {
            MENU_LABELS
                .into_iter()
                .map(|label| {
                    MenuEntry::bound(
                        label,
                        crate::testing::bound_press(
                            crate::testing::MENU_ROW_RESOURCE,
                            &crate::testing::MenuRows::action_of(label),
                        ),
                    )
                })
                .collect()
        });
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            area,
            (400, 400),
            crate::theme::Theme::dark(),
        );
        harness.relayout();
        harness.send(&crate::synthetic_input::cursor_moved(100.0, 100.0));
        (harness, chosen)
    }

    fn mouse(
        harness: &mut crate::headless::FrameHarness,
        state: ElementState,
        button: MouseButton,
    ) {
        harness.send(&crate::synthetic_input::mouse_input(state, button));
    }

    /// Right-click where the pointer rests, with the frame that rebuilds
    /// the changed content and paints the opened menu landing between press
    /// and release, as it does live.
    fn right_click(harness: &mut crate::headless::FrameHarness) {
        mouse(harness, ElementState::Pressed, MouseButton::Right);
        assert!(
            harness.root::<ContextMenuArea>().is_open(),
            "a right-click opens the menu"
        );
        harness.relayout();
        mouse(harness, ElementState::Released, MouseButton::Right);
        assert!(
            harness.root::<ContextMenuArea>().is_open(),
            "the release that ends the right-click leaves the menu open"
        );
    }

    /// The fourth entry is three rows down, as Canvas's Duplicate is: a
    /// keyboard row one off lands on another entry.
    #[test]
    fn a_right_click_menu_runs_the_entry_the_keyboard_chooses() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut harness, chosen) = four_entries();
        right_click(&mut harness);
        assert_eq!(
            harness.root::<ContextMenuArea>().menu().hovered(),
            Some(0),
            "the menu opens on its first entry, though the still pointer was replayed"
        );
        for _ in 0..3 {
            harness.key("ArrowDown");
        }
        harness.key("Enter");
        assert_eq!(*chosen.borrow(), vec!["Fourth"]);
        assert!(!harness.root::<ContextMenuArea>().is_open());
    }

    #[test]
    fn a_right_click_menu_runs_the_entry_the_pointer_clicks() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut harness, chosen) = four_entries();
        right_click(&mut harness);
        let second = harness
            .root::<ContextMenuArea>()
            .menu()
            .item_rect(1)
            .expect("the second entry is laid out")
            .center();
        harness.send(&crate::synthetic_input::cursor_moved(second.x, second.y));
        assert_eq!(
            harness.root::<ContextMenuArea>().menu().hovered(),
            Some(1),
            "hover follows the pointer once it moves"
        );
        mouse(&mut harness, ElementState::Pressed, MouseButton::Left);
        harness.relayout();
        mouse(&mut harness, ElementState::Released, MouseButton::Left);
        assert_eq!(*chosen.borrow(), vec!["Second"]);
        assert!(!harness.root::<ContextMenuArea>().is_open());
    }

    /// An open menu's entries are the typed actions behind them: the row names the app's action,
    /// an agent chooses it by activating the row's node as a screen reader does, and the app's
    /// action runs on its record and the menu closes. An agent's choice is the action, never a
    /// press where the row is drawn.
    #[test]
    fn an_agent_chooses_a_menu_entry_through_the_action_its_row_names() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut harness, chosen) = four_entries();
        right_click(&mut harness);
        let address = harness
            .root::<ContextMenuArea>()
            .menu()
            .items()
            .get(2)
            .and_then(MenuItem::action_address)
            .expect("the third entry names the action it runs");
        assert_eq!(
            address,
            tilo_actions::ActionAddress::new(crate::testing::MENU_ROW_RESOURCE, "third")
        );
        assert_eq!(
            harness
                .root::<ContextMenuArea>()
                .menu()
                .items()
                .get(2)
                .and_then(MenuItem::label),
            Some("Third")
        );
        let record = harness
            .resources()
            .into_iter()
            .find(|resource| resource.id.0 == crate::testing::MENU_ROW_RESOURCE)
            .expect("the app publishes the record its rows act on");
        let third = record
            .actions
            .iter()
            .find(|action| action.id == "third")
            .expect("the third action");
        assert_eq!(third.kind, tilo_actions::OperationKind::Effect);
        // The row node an agent reaches, under the area the menu stands over.
        let target = crate::a11y_child_route::encode(
            &[harness.root::<ContextMenuArea>().content().children().len()],
            Some("context-menu:item:2"),
        );
        harness
            .perform_semantic_named("MenuRows", Some(&target), &crate::a11y::Action::Activate)
            .expect("the entry is reachable");
        assert_eq!(
            *chosen.borrow(),
            vec!["Third".to_owned()],
            "the app's action ran on its record"
        );
        assert!(
            !harness.root::<ContextMenuArea>().is_open(),
            "the menu closed on its choice"
        );
    }

    /// Content that holds items, one of which has the keyboard: a list whose
    /// focused row stands at `anchor`.
    struct RowWithTheKeyboard {
        inner: Spacer,
        anchor: Rect,
    }

    impl Widget for RowWithTheKeyboard {
        fn build_layout(
            &mut self,
            taffy: &mut taffy::TaffyTree<()>,
            theme: &Theme,
            node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            self.inner.build_layout(taffy, theme, node_map)
        }
        fn apply_layout(
            &mut self,
            taffy: &taffy::TaffyTree<()>,
            node: taffy::NodeId,
            origin: Point,
        ) {
            self.inner.apply_layout(taffy, node, origin);
        }
        fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
            self.inner.draw(&mut crate::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ));
        }
        fn get_rect(&self) -> Rect {
            self.inner.get_rect()
        }
        fn name(&self) -> &'static str {
            "RowWithTheKeyboard"
        }
        fn is_focused(&self) -> bool {
            true
        }
        fn context_menu_anchor(&self) -> Option<Rect> {
            Some(self.anchor)
        }
    }

    /// The same menu on a display whose pixels are not the layout's: its
    /// edges fall on the pixels the frame draws, at every scale.
    #[test]
    fn a_keyboard_menu_lands_on_the_displays_pixels() {
        for scale in [1.0, 1.5, 2.0, 3.0] {
            // Motion reads at its end: this test checks the resting frame.
            let _settled = crate::motion::testing::settled();
            let anchor = Rect::new(20.3, 200.7, 300.6, 238.4);
            let area = ContextMenuArea::new(
                Box::new(RowWithTheKeyboard {
                    inner: Spacer::fixed(300.0),
                    anchor,
                }),
                |_| {
                    vec![MenuEntry::bound(
                        "First",
                        crate::testing::bound_press("test:menu", "first"),
                    )]
                },
            );
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                area,
                (400, 400),
                crate::theme::Theme::dark(),
            );
            harness.set_input_scale(scale).expect("a display scale");
            harness.relayout();
            harness.key("Shift+F10");
            harness.relayout();
            let rect = harness.root::<ContextMenuArea>().menu().get_rect();
            for edge in [rect.x0, rect.y0, rect.x1, rect.y1] {
                let pixels = edge * scale;
                assert!(
                    (pixels - pixels.round()).abs() < 0.01,
                    "{edge} is not a device pixel at {scale}x: {rect:?}"
                );
            }
        }
    }

    /// The reported capture's gesture: Menu/Shift+F10 on a focused row opens
    /// its menu under that row, not at the list's foot.
    #[test]
    fn the_keyboard_opens_the_menu_under_the_focused_row() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let anchor = Rect::new(20.0, 200.0, 300.0, 238.0);
        let area = ContextMenuArea::new(
            Box::new(RowWithTheKeyboard {
                inner: Spacer::fixed(300.0),
                anchor,
            }),
            |_| {
                vec![MenuEntry::bound(
                    "First",
                    crate::testing::bound_press("test:menu", "first"),
                )]
            },
        );
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            area,
            (400, 400),
            crate::theme::Theme::dark(),
        );
        // The frame that gives the area the window its menu stands in.
        harness.relayout();
        harness.key("Shift+F10");
        assert!(
            harness.root::<ContextMenuArea>().is_open(),
            "Shift+F10 on the focused item opens its menu"
        );
        harness.relayout();
        let rect = harness.root::<ContextMenuArea>().menu().get_rect();
        let gap = f64::from(crate::tokens::Spacing::BASE.s1);
        assert!(
            rect.y0 >= anchor.y1 && rect.y0 - anchor.y1 <= gap + 0.5,
            "the menu stands under the row it opened from: {rect:?} vs {anchor:?}"
        );
    }

    /// A widget's own action is offered beside its owner's rows where the person right-clicks
    /// it, and choosing it runs on the widget itself: a link's Copy link copies its address and
    /// opens nothing.
    #[test]
    fn a_link_under_the_pointer_offers_its_copy_beside_the_owners_rows() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        use crate::clipboard::{ClipboardBackend as _, ClipboardTarget, InProcessClipboard};
        let clipboard = std::sync::Arc::new(InProcessClipboard::default());
        let services = crate::process::ProcessServices::null().with_clipboard(clipboard.clone());
        crate::process::scoped(&services, || {
            let area = ContextMenuArea::new(
                Box::new(
                    crate::widget::Link::new("https://example.org/page").with_label("The page"),
                ),
                |_| {
                    vec![MenuEntry::bound(
                        "Open",
                        crate::testing::bound_press("test:menu", "open"),
                    )]
                },
            );
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                area,
                (400, 120),
                crate::theme::Theme::dark(),
            );
            harness.relayout();
            let page = harness
                .root::<ContextMenuArea>()
                .content()
                .get_rect()
                .center();
            harness.send(&crate::synthetic_input::cursor_moved(page.x, page.y));
            right_click(&mut harness);
            let labels: Vec<String> = harness
                .root::<ContextMenuArea>()
                .menu()
                .items()
                .iter()
                .filter_map(MenuItem::label)
                .map(str::to_owned)
                .collect();
            assert_eq!(
                labels
                    .iter()
                    .take(2)
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                ["Open", "Copy link"],
                "the link's copy stands beside what its row offers"
            );
            let copy = harness
                .root::<ContextMenuArea>()
                .menu()
                .item_rect(1)
                .expect("the copy row is laid out")
                .center();
            harness.send(&crate::synthetic_input::cursor_moved(copy.x, copy.y));
            mouse(&mut harness, ElementState::Pressed, MouseButton::Left);
            harness.relayout();
            mouse(&mut harness, ElementState::Released, MouseButton::Left);
            assert_eq!(
                clipboard.text(ClipboardTarget::Clipboard).as_deref(),
                Some("https://example.org/page"),
                "choosing the copy put the address it goes to on the clipboard"
            );
            assert!(
                !harness.root::<ContextMenuArea>().is_open(),
                "the menu closed after its choice"
            );
        });
    }

    /// A widget that offers no action of its own adds no row: the menu is its owner's alone.
    #[test]
    fn a_widget_with_no_own_action_adds_no_row() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut harness, _) = four_entries();
        right_click(&mut harness);
        let labels: Vec<String> = harness
            .root::<ContextMenuArea>()
            .menu()
            .items()
            .iter()
            .filter_map(MenuItem::label)
            .map(str::to_owned)
            .collect();
        assert_eq!(
            labels
                .iter()
                .take(4)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["First", "Second", "Third", "Fourth"]
        );
    }

    #[test]
    fn nothing_to_offer_opens_nothing() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), |_| {
            vec![MenuEntry::separator()]
        });
        assert!(!area.open_at(Point::new(5.0, 5.0), true));
        assert!(!area.is_open());
    }

    /// A row whose menu the host has not answered yet opens no menu: not even the rows the menu
    /// adds itself — the one that asks what is typed, and the send row after it — which would
    /// show a menu of nothing and never take the row's commands. It opens where it was asked
    /// once the row comes.
    #[test]
    fn a_menu_waiting_for_its_row_opens_on_the_row_and_not_on_the_rows_it_adds() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        crate::shared_selection::set_send_host_available(true);
        let arrived = Rc::new(Cell::new(false));
        let area = ContextMenuArea::new(Box::new(Spacer::fixed(300.0)), {
            let arrived = Rc::clone(&arrived);
            move |_| {
                if arrived.get() {
                    vec![MenuEntry::bound(
                        "Delete",
                        crate::testing::bound_press("test:menu", "delete"),
                    )]
                } else {
                    Vec::new()
                }
            }
        })
        .waiting_on({
            let arrived = Rc::clone(&arrived);
            move |_, _| !arrived.get()
        });
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            area,
            (400, 400),
            crate::theme::Theme::dark(),
        );
        harness.relayout();
        let at = Point::new(100.0, 100.0);
        let press = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
        crate::widget::context_menu::note_input(&press);
        harness.send(&crate::synthetic_input::cursor_moved(at.x, at.y));
        harness.send(&press);
        assert!(
            !harness.root::<ContextMenuArea>().is_open(),
            "the menu waits for the row rather than opening on the rows it adds itself"
        );
        // The app answers with the row, and the frame that lays it out opens the menu on it.
        arrived.set(true);
        harness.relayout();
        let area = harness.root::<ContextMenuArea>();
        assert!(area.is_open(), "the menu opened on the row that came");
        assert_eq!(
            area.menu().items().first().and_then(MenuItem::label),
            Some("Delete")
        );
        crate::shared_selection::set_send_host_available(false);
    }
}
