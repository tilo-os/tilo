//! A context menu over any element: right-click it, or press the Menu key
//! or Shift+F10 while focus is inside it, and the shared [`ContextMenu`]
//! opens with the commands its owner offers there and then.
//!
//! The entries are built when the menu opens, so they reflect what is
//! true at that moment (what is selected, which layer the row is), and
//! each runs a closure on the UI thread: an app dispatches its actions
//! from there, the same actions its buttons and keys use.

use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::{ContextMenu, Element, EventCtx, MenuItem, Widget};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// One row of a context menu: what it shows, and what choosing it does.
pub struct MenuEntry {
    item: MenuItem,
    run: Option<Rc<dyn Fn()>>,
    /// What choosing it presses, when the view bound it: the action runs through the area's
    /// custody, so a press of the row and anyone else running the action take the one path.
    press: crate::widget::Press,
    /// An emoji bar's way to the emoji picker: set when its last cell was chosen, and where
    /// the picked emoji goes.
    more: Option<(Rc<std::cell::Cell<bool>>, crate::component::Event)>,
}

impl MenuEntry {
    /// A command: `label` shown, `run` called when it is chosen.
    pub fn new(label: impl Into<String>, run: impl Fn() + 'static) -> Self {
        Self {
            item: MenuItem::selection(label),
            run: Some(Rc::new(run)),
            press: crate::widget::Press::default(),
            more: None,
        }
    }

    /// A command that takes something away, styled as such.
    pub fn destructive(label: impl Into<String>, run: impl Fn() + 'static) -> Self {
        Self {
            item: MenuItem::destructive_selection(label),
            run: Some(Rc::new(run)),
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
                item
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

/// The entries a menu offers where it opens, at the pointer in window
/// coordinates (`None` when the keyboard opened it).
pub type MenuEntries = Box<dyn Fn(Option<Point>) -> Vec<MenuEntry>>;

/// The entries a menu offers where it opens, asked of the content: a list
/// answers for the row under the pointer, or its focused row when the
/// keyboard opened the menu (`None`).
pub type ContentMenuEntries = Box<dyn Fn(&dyn Widget, Option<Point>) -> Vec<MenuEntry>>;

/// What opened the open menu: where it must keep standing.
#[derive(Clone, Copy)]
enum Opening {
    /// The pointer, at that point.
    Pointer(Point),
    /// The keyboard, on the item that has it: read again every frame, so the
    /// menu follows the row through a scroll, a resize or a text scale.
    Item,
}

#[crate::component::not_a_component(part, "what the `ui.ContextMenu` attachment wraps its view in")]
pub struct ContextMenuArea {
    content: Element,
    entries: ContentMenuEntries,
    menu: ContextMenu,
    /// What opened the open menu, and so where it stands.
    opening: Option<Opening>,
    runs: Vec<Option<Rc<dyn Fn()>>>,
    /// What each entry presses, in step with `runs`.
    presses: Vec<crate::widget::Press>,
    cursor: Point,
    /// The pointer's last move, replayed to the content once a command
    /// ran, so what the command asked of it (a rename beginning) happens
    /// at once rather than at the pointer's next move.
    moved: Option<WindowEvent>,
    /// The entry pressed with the pointer, run when the button comes up.
    pending: Option<usize>,
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
            menu: ContextMenu::new(),
            opening: None,
            runs: Vec::new(),
            presses: Vec::new(),
            cursor: Point::ZERO,
            moved: None,
            pending: None,
            viewport: None,
            more: Vec::new(),
            picker: None,
            picker_wanted: None,
            environment: crate::interaction::InteractionEnvironment::default(),
        }
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
        let entries = (self.entries)(self.content.as_ref(), at_pointer);
        let offered = entries.iter().any(|entry| entry.run.is_some());
        let mut menu = ContextMenu::new();
        self.runs.clear();
        self.presses.clear();
        self.more.clear();
        if offered {
            for entry in entries {
                menu.push(entry.item);
                self.runs.push(entry.run);
                self.presses.push(entry.press);
                self.more.extend(entry.more);
            }
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
        // Nothing offered, and no agent to tell: no menu.
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

    /// Run the chosen entry, then let the content see the pointer where
    /// it is, so what the command asked of it happens now.
    fn run(&mut self, index: usize, ctx: &mut EventCtx<'_>) {
        let place = self.menu.get_rect();
        self.choose(index);
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
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if (matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                || (matches!(logical_key, Key::Named(NamedKey::F10))
                    && ctx.modifiers.state().shift_key()))
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
pub(super) fn keyboard_anchor(widget: &dyn Widget) -> Option<Rect> {
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
/// the open menu: a row an agent chooses is pressed where it is drawn.
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

    // The area answers operations itself, so one addressed to a row of its
    // open menu reaches the menu even when the content answers its own.
    // Every other capability is the content's.
    fn provide<'a>(&'a self, request: &mut super::capability::Request<'a, '_>) {
        if request.wants::<dyn super::ResourceOperations>() {
            request.provide::<dyn super::ResourceOperations>(self);
            return;
        }
        self.content.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut super::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn super::ResourceOperations>() {
            request.provide::<dyn super::ResourceOperations>(self);
            return;
        }
        self.content.provide_mut(request);
    }

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
        // The press the menu took is the row the person chose, run as the row's own control
        // runs: the release that follows belongs to that press, and is the menu's, so what the
        // command started (a rename's field) never sees half a click.
        if self.pending.is_some() {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                }
            ) {
                self.pending = None;
                *ctx.handled = true;
                return;
            }
            if matches!(event, WindowEvent::MouseInput { .. }) {
                self.pending = None;
            }
        }
        if !self.menu.is_open() && self.menu.send_to_alone_open() {
            self.menu.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        if self.menu.is_open() {
            // The menu itself tells the host's replay of the still opening
            // pointer from a real move, so the keyboard keeps its row.
            self.menu.handle_event(event, ctx);
            if let Some(index) = self.menu.take_selection() {
                self.menu.close();
                // A press runs its row here and keeps the release that follows; a key's row
                // runs at once, as the key itself does.
                if matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                ) {
                    self.pending = Some(index);
                }
                self.run(index, ctx);
                *ctx.handled = true;
            }
            if self.menu.is_open() || *ctx.handled {
                return;
            }
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
            if self.open_over(anchor, at_pointer) {
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
        self.menu.layout_at_position();
    }

    fn wants_pointer_capture(&self) -> bool {
        self.menu.is_showing()
            || self.pending.is_some()
            || self.picker.is_some()
            || self.content.wants_pointer_capture()
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
    /// and a picker it stands open. A box standing without the menu is the
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

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The overlay pass sees the whole window, where the menu may go;
        // the content's own pass may be scoped to a scrolled list.
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
            // The row it stands under may have scrolled, and the window may
            // have changed: the menu stands where what opened it is now.
            self.place_menu();
        }
        // A box the window or the shell asked for with no menu around it:
        // it stands where its anchor does, until it sends.
        if let Some(box_) = crate::shared_selection::take_open_send_box()
            && !self.menu.is_open()
        {
            if self.viewport.is_none() {
                self.viewport = presence.viewport.get();
            }
            self.menu.set_viewport(self.viewport);
            self.menu
                .open_send_to_alone(box_.about, box_.bounds, box_.draft.as_deref(), &box_.to);
        }
        self.content
            .draw_overlay(scene, transform, text_renderer, theme, presence);
        // The menu paints in the overlay pass, over everything.
        if self.menu.is_drawn() || self.menu.send_to_alone_drawn() {
            self.menu
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if let Some((anchor, pick)) = self.picker_wanted.take() {
            let mut picker = super::EmojiPicker::framed();
            let viewport = self.viewport.unwrap_or_else(|| self.content.get_rect());
            picker.open_at(Some(anchor), viewport, theme);
            self.picker = Some((picker, pick));
        }
        if let Some((picker, _)) = self.picker.as_mut() {
            picker.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    // An open menu is part of the tree, so assistive technology and
    // inspection see it.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.content.children();
        if self.menu.is_open() {
            children.push(&self.menu);
        }
        if let Some((picker, _)) = &self.picker {
            children.push(picker);
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        self.content.for_each_child(visitor);
        if self.menu.is_open() {
            visitor(&self.menu);
        }
    }

    /// This area stands for its content's node, laid out under it.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(self.content.as_ref());
        visitor(&self.menu);
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let open = self.menu.is_open();
        let mut children = self.content.children_mut();
        if open {
            children.push(&mut self.menu);
        }
        children
    }
}

impl crate::widget::controls::ActionControls for ContextMenuArea {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        crate::widget::controls::install_control_custody(&mut *self.content, domain, attachment);
        for press in &self.presses {
            press.install_custody(domain, attachment);
        }
        crate::widget::controls::install_control_custody(&mut self.menu, domain, attachment);
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

    use super::*;
    use crate::widget::Spacer;

    fn moved(at: Point) -> WindowEvent {
        crate::synthetic_input::cursor_moved(at.x, at.y)
    }

    fn right_release() -> WindowEvent {
        crate::synthetic_input::mouse_input(ElementState::Released, MouseButton::Right)
    }

    fn opening_menu() -> (ContextMenuArea, Rc<Cell<usize>>, Point) {
        let chosen = Rc::new(Cell::new(0));
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), {
            let chosen = Rc::clone(&chosen);
            move |_| {
                (1..=4)
                    .map(|index| {
                        let chosen = Rc::clone(&chosen);
                        MenuEntry::new(format!("Action {index}"), move || chosen.set(index))
                    })
                    .collect()
            }
        });
        let at = Point::new(20.0, 20.0);
        crate::testing::dispatch(&mut area, &moved(at));
        assert!(area.open_at(at, true));
        (area, chosen, at)
    }

    fn named_key(key: NamedKey) -> WindowEvent {
        crate::synthetic_input::key_input(&Key::Named(key), ElementState::Pressed)
    }

    #[test]
    fn movement_during_opening_keeps_the_pointer_row_for_enter() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut area, chosen, _) = opening_menu();
        let Some(row) = area.menu().item_rect(1) else {
            panic!("the second action must have a hit target");
        };
        crate::testing::dispatch(&mut area, &moved(row.center()));
        crate::testing::dispatch(&mut area, &right_release());
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &named_key(NamedKey::Enter));
        assert_eq!(chosen.get(), 2);
    }

    #[test]
    fn keyboard_choice_before_opening_release_survives_stationary_replay() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut area, chosen, at) = opening_menu();
        crate::testing::dispatch(&mut area, &named_key(NamedKey::ArrowDown));
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &moved(at));
        crate::testing::dispatch(&mut area, &right_release());
        assert_eq!(area.menu().hovered(), Some(1));
        crate::testing::dispatch(&mut area, &named_key(NamedKey::Enter));
        assert_eq!(chosen.get(), 2);
    }

    #[test]
    fn a_menu_over_its_content_is_asked_with_the_content() {
        let mut area = ContextMenuArea::over_content(
            Box::new(Spacer::fixed(10.0).with_id("rows")),
            |content, at| {
                let named = content.test_id().unwrap_or_default().to_owned();
                vec![MenuEntry::new(format!("{named} {}", at.is_some()), || {})]
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
        let chosen = Rc::new(Cell::new(0));
        let offered = Rc::new(Cell::new(1));
        let mut area = ContextMenuArea::new(Box::new(Spacer::fixed(10.0)), {
            let chosen = Rc::clone(&chosen);
            let offered = Rc::clone(&offered);
            move |_| {
                let n = offered.get();
                let chosen = Rc::clone(&chosen);
                vec![
                    MenuEntry::new(format!("Take {n}"), move || chosen.set(n))
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
        assert_eq!(chosen.get(), 7);
        assert!(!area.is_open());
    }

    type Chosen = Rc<std::cell::RefCell<Vec<&'static str>>>;

    /// Content that a right-click changes, as a canvas picking the mark
    /// under it does: the next frame rebuilds the tree.
    struct PicksOnRightPress {
        inner: Spacer,
        picked: bool,
    }

    impl Widget for PicksOnRightPress {
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
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.inner
                .draw(scene, transform, text_renderer, theme, presence);
        }
        fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut EventCtx<'_>) {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                    ..
                }
            ) {
                self.picked = true;
            }
        }
        fn take_structure_dirty(&mut self) -> bool {
            std::mem::take(&mut self.picked)
        }
        fn get_rect(&self) -> Rect {
            self.inner.get_rect()
        }
        fn name(&self) -> &'static str {
            "PicksOnRightPress"
        }
    }

    /// A 300 px area offering First to Fourth, in a window whose host
    /// replays a still pointer after the widgets change, the pointer resting
    /// at (100, 100).
    fn four_entries() -> (crate::headless::FrameHarness, Chosen) {
        let chosen = Chosen::default();
        let content = PicksOnRightPress {
            inner: Spacer::fixed(300.0),
            picked: false,
        };
        let area = ContextMenuArea::new(Box::new(content), {
            let chosen = Rc::clone(&chosen);
            move |_| {
                ["First", "Second", "Third", "Fourth"]
                    .into_iter()
                    .map(|label| {
                        let chosen = Rc::clone(&chosen);
                        MenuEntry::new(label, move || chosen.borrow_mut().push(label))
                    })
                    .collect()
            }
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

    /// An open menu's entries are its published controls: an agent chooses
    /// one as a person does, and the area runs the entry it read from the
    /// menu's choice.
    #[test]
    fn an_agent_chooses_a_menu_entry_through_its_published_control() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let (mut harness, chosen) = four_entries();
        assert!(
            !harness
                .resources()
                .iter()
                .any(|resource| resource.id.0.starts_with("tilo.control:menu.")),
            "a closed menu offers nothing"
        );
        right_click(&mut harness);
        let resources = harness.resources();
        let third = resources
            .iter()
            .find(|resource| resource.id.0 == "tilo.control:menu.2")
            .expect("the third entry is published");
        assert_eq!(third.label, "Third");
        assert_eq!(third.actions[0].kind, tilo_actions::OperationKind::Effect);
        harness
            .invoke_published("tilo.control:menu.2", "invoke", &serde_json::Map::new())
            .expect("the entry is reachable");
        assert_eq!(*chosen.borrow(), vec!["Third"]);
        assert!(!harness.root::<ContextMenuArea>().is_open());
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
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.inner
                .draw(scene, transform, text_renderer, theme, presence);
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
                |_| vec![MenuEntry::new("First", || {})],
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
            |_| vec![MenuEntry::new("First", || {})],
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
}
